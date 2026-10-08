use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::Path,
    sync::Mutex,
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{GetLastError, SetLastError},
        System::{
            Diagnostics::Debug::OutputDebugStringW,
            SystemInformation::GetLocalTime,
            SystemServices::TIME_ZONE_ID_DAYLIGHT,
            Threading::GetCurrentThreadId,
            Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION},
        },
    },
};

const MAX_FILE: u64 = 100 * 1024;
const MAX_MESSAGE: usize = 16 * 1024;
pub struct Sink {
    file: File,
    bytes: u64,
}
impl Sink {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        let mut bytes = file.metadata()?.len();
        if bytes > MAX_FILE {
            file.set_len(0)?;
            bytes = 0;
        }
        file.seek(SeekFrom::End(0))?;
        Ok(Self { file, bytes })
    }
    pub fn write(&mut self, line: &str) -> std::io::Result<()> {
        if self.bytes + line.len() as u64 > MAX_FILE {
            self.file.set_len(0)?;
            self.file.seek(SeekFrom::Start(0))?;
            self.bytes = 0;
        }
        self.file.write_all(line.as_bytes())?;
        self.bytes += line.len() as u64;
        self.file.flush()
    }
}
struct Logger {
    sink: Mutex<Option<Sink>>,
}
static LOGGER: std::sync::OnceLock<Logger> = std::sync::OnceLock::new();
pub fn init() {
    let logger = LOGGER.get_or_init(|| Logger {
        sink: Mutex::new(
            std::env::current_exe()
                .ok()
                .and_then(|exe| Sink::open(&exe.with_file_name("AxonkeyService.log")).ok()),
        ),
    });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}
fn sanitized(message: &str) -> String {
    let mut end = message.len().min(MAX_MESSAGE);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    let mut text = message[..end].replace(['\r', '\n', '\0'], " ");
    if end < message.len() {
        text.push_str(" [truncated]");
    }
    text
}
pub fn debug(message: &str) {
    let wide = super::win::handles::wide(message);
    // SAFETY: null-terminated UTF-16 lives for the entire call.
    unsafe {
        OutputDebugStringW(PCWSTR(wide.as_ptr()));
    }
}
impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // SAFETY: these APIs read thread-local error/time information into initialized storage.
        let last = unsafe { GetLastError() };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: GetLocalTime/GetCurrentThreadId have no preconditions.
            let (time, tid) = unsafe { (GetLocalTime(), GetCurrentThreadId()) };
            let mut zone = TIME_ZONE_INFORMATION::default();
            // SAFETY: zone is a valid, writable structure.
            let id = unsafe { GetTimeZoneInformation(&mut zone) };
            let bias = -(zone.Bias
                + if id == TIME_ZONE_ID_DAYLIGHT {
                    zone.DaylightBias
                } else if id == 1 {
                    zone.StandardBias
                } else {
                    0
                });
            let level = match record.level() {
                log::Level::Error => "error",
                log::Level::Warn => "warning",
                _ => "info",
            };
            let line = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}{}{:02}:{:02} [{level}] [AxonkeyService] [pid={} tid={tid}] {}\n",
                time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond, time.wMilliseconds,
                if bias >= 0 { '+' } else { '-' }, bias.abs() / 60, bias.abs() % 60, std::process::id(), sanitized(&record.args().to_string()));
            debug(&line);
            if let Some(sink) = crate::lock(&self.sink).as_mut() {
                if sink.write(&line).is_err() {
                    debug("AxonkeyService file logging failed\n");
                }
            }
        }));
        // SAFETY: restore the error captured on this same thread.
        unsafe {
            SetLastError(last);
        }
    }
    fn flush(&self) {
        if let Some(sink) = crate::lock(&self.sink).as_mut() {
            let _ = sink.file.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_bounds_unicode_restart_and_utf8_truncation() {
        let path = std::env::temp_dir().join(format!("axonkey-日志-{}.log", std::process::id()));
        {
            let mut sink = Sink::open(&path).unwrap();
            sink.write("中文\n").unwrap();
        }
        {
            let mut sink = Sink::open(&path).unwrap();
            sink.write("append\n").unwrap();
        }
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("中文\nappend\n"));
        {
            let mut sink = Sink::open(&path).unwrap();
            for _ in 0..20 {
                sink.write(&format!("{}\n", "x".repeat(10000))).unwrap();
            }
        }
        assert!(std::fs::metadata(&path).unwrap().len() <= MAX_FILE);
        assert!(sanitized(&"中".repeat(10000)).ends_with(" [truncated]"));
        assert_eq!(sanitized("a\r\nb\0"), "a  b ");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn concurrent_logging_preserves_lines_and_callers_last_error() {
        let path = std::env::temp_dir().join(format!("axonkey-threads-{}.log", std::process::id()));
        let logger = Logger {
            sink: Mutex::new(Some(Sink::open(&path).unwrap())),
        };
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let logger = &logger;
                scope.spawn(move || {
                    for _ in 0..20 {
                        // SAFETY: manipulating only this test worker's thread-local Win32 error.
                        unsafe {
                            SetLastError(windows::Win32::Foundation::WIN32_ERROR(1234));
                        }
                        log::Log::log(
                            logger,
                            &log::Record::builder()
                                .level(log::Level::Warn)
                                .args(format_args!("并发完整行"))
                                .build(),
                        );
                        // SAFETY: no preconditions; inspect the same thread's error.
                        assert_eq!(unsafe { GetLastError().0 }, 1234);
                    }
                });
            }
        });
        drop(logger);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 80);
        assert!(text.lines().all(
            |line| line.contains("[warning] [AxonkeyService]") && line.ends_with("并发完整行")
        ));
        std::fs::remove_file(path).unwrap();
        assert!(Sink::open(&std::env::temp_dir()).is_err());
    }
}
