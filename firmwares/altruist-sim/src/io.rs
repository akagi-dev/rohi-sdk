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

//! Small async helpers over `async-io` sockets.

use std::io::{ErrorKind, Read as _, Write as _};
use std::net::TcpStream;

use async_io::Async;

/// Reads what is available into `buf`; 0 is the end of the stream.
pub async fn read_some(stream: &Async<TcpStream>, buf: &mut [u8]) -> std::io::Result<usize> {
    stream
        .read_with(|stream| {
            let mut stream = stream;
            stream.read(buf)
        })
        .await
}

/// Writes all of `data`.
pub async fn write_all(stream: &Async<TcpStream>, mut data: &[u8]) -> std::io::Result<()> {
    while !data.is_empty() {
        let written = stream
            .write_with(|stream| {
                let mut stream = stream;
                stream.write(data)
            })
            .await?;
        if written == 0 {
            return Err(ErrorKind::WriteZero.into());
        }
        data = &data[written..];
    }
    Ok(())
}
