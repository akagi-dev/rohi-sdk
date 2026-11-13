///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025-2026 Akagi Engineering <admin@akagi.dev>
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
//
///////////////////////////////////////////////////////////////////////////////
//! A linked ELF image reduced to what the analysis needs.

use object::{Object, ObjectSection, ObjectSymbol, SectionKind, SymbolKind, SymbolSection};

/// Failure to read an image.
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    /// The file is not a parseable ELF.
    #[error("cannot parse ELF: {0}")]
    Parse(#[from] object::Error),
    /// The `.stack_sizes` section is malformed.
    #[error("malformed .stack_sizes: {0}")]
    StackSizes(&'static str),
}

/// A section of the image.
#[derive(Debug, Clone)]
pub struct Section {
    /// Name.
    pub name: String,
    /// Address.
    pub address: u64,
    /// Size.
    pub size: u64,
    /// `true` for sections without file content (`.bss`, `.stack`).
    pub nobits: bool,
}

/// What a symbol is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Code.
    Func,
    /// Data.
    Object,
    /// Anything else, including linker-defined absolute symbols.
    Other,
}

/// A symbol of the image.
#[derive(Debug, Clone)]
pub struct Sym {
    /// Raw (mangled) name.
    pub raw: String,
    /// Address.
    pub address: u64,
    /// Size, `0` when unknown.
    pub size: u64,
    /// Kind.
    pub kind: Kind,
    /// Name of the section, when the symbol belongs to one.
    pub section: Option<String>,
}

/// The parsed image.
#[derive(Debug, Clone, Default)]
pub struct Image {
    /// Sections in file order.
    pub sections: Vec<Section>,
    /// Symbols in table order.
    pub symbols: Vec<Sym>,
    /// `(function address, frame bytes)`; `None` when the image has no `.stack_sizes`.
    pub stack_sizes: Option<Vec<(u64, u64)>>,
}

impl Image {
    /// Parses `data` as an ELF image.
    pub fn parse(data: &[u8]) -> Result<Image, ImageError> {
        let file = object::File::parse(data)?;
        let mut sections = Vec::new();
        let mut stack_sizes = None;
        for section in file.sections() {
            let name = section.name().unwrap_or_default().to_string();
            if name == ".stack_sizes" {
                let bytes = section.data()?;
                stack_sizes = Some(
                    parse_stack_sizes(bytes, file.is_64())?
                        .into_iter()
                        .chain(stack_sizes.take().unwrap_or_default())
                        .collect(),
                );
                continue;
            }
            sections.push(Section {
                name,
                address: section.address(),
                size: section.size(),
                nobits: section.kind() == SectionKind::UninitializedData
                    || section.kind() == SectionKind::UninitializedTls,
            });
        }
        let names: std::collections::HashMap<_, _> = file
            .sections()
            .filter_map(|s| Some((s.index(), s.name().ok()?.to_string())))
            .collect();
        let symbols = file
            .symbols()
            .filter(|s| s.name().is_ok_and(|n| !n.is_empty()))
            .map(|s| Sym {
                raw: s.name().unwrap_or_default().to_string(),
                address: s.address(),
                size: s.size(),
                kind: match s.kind() {
                    SymbolKind::Text => Kind::Func,
                    SymbolKind::Data | SymbolKind::Tls => Kind::Object,
                    _ => Kind::Other,
                },
                section: match s.section() {
                    SymbolSection::Section(i) => names.get(&i).cloned(),
                    _ => None,
                },
            })
            .collect();
        Ok(Image {
            sections,
            symbols,
            stack_sizes,
        })
    }

    /// The section called `name`.
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.name == name)
    }

    /// The address of the first symbol called `name`.
    pub fn symbol_address(&self, name: &str) -> Option<u64> {
        self.symbols
            .iter()
            .find(|s| s.raw == name)
            .map(|s| s.address)
    }
}

/// Parses the concatenated `(address, ULEB128 size)` records of `.stack_sizes`.
fn parse_stack_sizes(mut bytes: &[u8], is_64: bool) -> Result<Vec<(u64, u64)>, ImageError> {
    let width = if is_64 { 8 } else { 4 };
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let (addr, rest) = bytes
            .split_at_checked(width)
            .ok_or(ImageError::StackSizes("truncated address"))?;
        let mut raw = [0u8; 8];
        raw[..width].copy_from_slice(addr);
        let address = u64::from_le_bytes(raw);
        let (size, used) = uleb128(rest).ok_or(ImageError::StackSizes("truncated size"))?;
        out.push((address, size));
        bytes = &rest[used..];
    }
    Ok(out)
}

fn uleb128(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (i, byte) in bytes.iter().enumerate() {
        let shift = u32::try_from(i).ok()?.checked_mul(7)?;
        if shift >= 64 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uleb_decodes_multi_byte_values() {
        assert_eq!(uleb128(&[0x7f]), Some((127, 1)));
        assert_eq!(uleb128(&[0x80, 0x01]), Some((128, 2)));
        assert_eq!(uleb128(&[0x80]), None);
    }

    #[test]
    fn stack_sizes_split_into_records() {
        let bytes = [0x10, 0, 0, 0, 0x20, 0x20, 0, 0, 0, 0x80, 0x02];
        assert_eq!(
            parse_stack_sizes(&bytes, false).ok(),
            Some(vec![(0x10, 0x20), (0x20, 0x100)])
        );
        assert!(parse_stack_sizes(&bytes[..3], false).is_err());
    }
}
