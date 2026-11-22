use windows::Win32::System::SystemInformation::GetLocalTime;

/// The current local date and time to the millisecond, as `2026-01-31 14:05:09.123`,
/// for stamping log lines.
pub fn local_timestamp() -> String {
    // SAFETY: the call takes no arguments and has no preconditions.
    let now = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond, now.wMilliseconds
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_has_a_fixed_shape() {
        let stamp = local_timestamp();
        let shape: String = stamp
            .chars()
            .map(|character| {
                if character.is_ascii_digit() {
                    '0'
                } else {
                    character
                }
            })
            .collect();

        assert_eq!(shape, "0000-00-00 00:00:00.000");
    }
}
