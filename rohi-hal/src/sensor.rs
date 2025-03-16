///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025 Akagi Engineering <research@akagi.dev>
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
//! Different kinds of sensors and how to access it's data. 

/// A Particulate Matter (PM) Sensor measures the floating particles in the air.
pub trait ParticulateMatter: Sized {
    /// Errors happens, be ready.
    type Error;

    /// Measure PM value.
    fn measure(&self) -> Result<Self, Self::Error>;
}
