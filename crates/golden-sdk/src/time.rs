//! Время и паузы без `std`.
//!
//! SDK не тянет `std::time`, но стенду и тестам нужны миллисекунды: таймауты
//! ожидания окна, паузы между попытками, метки в отчётах.

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetTickCount64() -> u64;
    fn Sleep(milliseconds: u32);
}

/// Миллисекунды с момента загрузки системы (монотонные).
pub fn now_ms() -> u64 {
    // SAFETY: вызов без аргументов и без побочных эффектов.
    unsafe { GetTickCount64() }
}

/// Спит `milliseconds` миллисекунд.
pub fn sleep_ms(milliseconds: u32) {
    // SAFETY: вызов без аргументов-указателей.
    unsafe { Sleep(milliseconds) }
}