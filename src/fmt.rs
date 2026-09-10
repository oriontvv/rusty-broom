//! Human-readable byte sizes.

const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

/// `1536` -> `"1.5 KiB"`. Keeps three significant digits so columns stay narrow.
pub fn bytes(value: u64) -> String {
    if value < 1024 {
        return format!("{value} B");
    }
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    let precision = if size >= 100.0 {
        0
    } else if size >= 10.0 {
        1
    } else {
        2
    };
    format!("{size:.precision$} {}", UNITS[unit])
}

/// Same as [`bytes`], but renders an unknown size as a placeholder.
pub fn maybe_bytes(value: Option<u64>) -> String {
    match value {
        Some(v) => bytes(v),
        None => "—".to_string(),
    }
}

/// `"500M"`, `"1.5GiB"`, `"2048"` -> bytes. Suffixes are binary multiples.
pub fn parse_bytes(input: &str) -> anyhow::Result<u64> {
    let text = input.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != ',')
        .unwrap_or(text.len());
    let (number, suffix) = text.split_at(split);
    let number: f64 = number
        .replace(',', ".")
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid size {input:?}"))?;
    if number < 0.0 {
        anyhow::bail!("size {input:?} must not be negative");
    }

    let multiplier: u64 = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1u64 << 40,
        other => anyhow::bail!("unknown size unit {other:?} in {input:?}"),
    };
    Ok((number * multiplier as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1024), "1.00 KiB");
        assert_eq!(bytes(1536), "1.50 KiB");
        assert_eq!(bytes(20 * 1024), "20.0 KiB");
        assert_eq!(bytes(500 * 1024), "500 KiB");
        assert_eq!(bytes(4 * 1024 * 1024 * 1024), "4.00 GiB");
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_bytes("0").unwrap(), 0);
        assert_eq!(parse_bytes("2048").unwrap(), 2048);
        assert_eq!(parse_bytes("1k").unwrap(), 1024);
        assert_eq!(parse_bytes("500M").unwrap(), 500 * (1 << 20));
        assert_eq!(parse_bytes("1.5GiB").unwrap(), 1610612736);
        assert!(parse_bytes("big").is_err());
        assert!(parse_bytes("10x").is_err());
    }
}
