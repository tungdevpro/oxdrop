/// Formats a byte count the way phones and Finder do (powers of 1000), e.g. `2.5 KB`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::format_size;

    #[test]
    fn formats_sizes_in_powers_of_1000() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1000), "1.0 KB");
        assert_eq!(format_size(2_500), "2.5 KB");
        assert_eq!(format_size(999_960), "1.0 MB");
        assert_eq!(format_size(3_000_000), "3.0 MB");
        assert_eq!(format_size(4_700_000_000), "4.7 GB");
        assert_eq!(format_size(u64::MAX), "18446.7 PB");
    }
}
