use std::{fmt, io};

#[derive(Debug, Clone)]
pub struct Error {
    pub native: u32,
    pub message: String,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn new(native: u32, message: impl Into<String>) -> Self {
        Self {
            native,
            message: message.into(),
        }
    }
    pub fn last(message: &str) -> Self {
        // SAFETY: GetLastError has no preconditions; capture before formatting/logging.
        let native = unsafe { windows::Win32::Foundation::GetLastError().0 };
        Self::new(native, message)
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (native=0x{:08X})", self.message, self.native)
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::new(value.raw_os_error().unwrap_or(31) as u32, value.to_string())
    }
}
impl From<windows::core::Error> for Error {
    fn from(value: windows::core::Error) -> Self {
        Self::new(value.code().0 as u32, value.message())
    }
}
