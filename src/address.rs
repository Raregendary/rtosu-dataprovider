use anyhow::{Result, bail};

pub fn parse_u64(value: &str) -> Result<u64> {
    let value = value.trim();
    if value.is_empty() {
        bail!("address cannot be empty");
    }

    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return Ok(u64::from_str_radix(hex, 16)?);
    }

    Ok(value.parse::<u64>()?)
}

pub fn parse_u128_as_u64(value: &str) -> Result<u64> {
    let value = value.trim();
    if value.is_empty() {
        bail!("address cannot be empty");
    }

    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        let parsed = u128::from_str_radix(hex, 16)?;
        return u64::try_from(parsed).map_err(|_| anyhow::anyhow!("address does not fit in u64"));
    }

    let parsed = value.parse::<u128>()?;
    u64::try_from(parsed).map_err(|_| anyhow::anyhow!("address does not fit in u64"))
}

pub fn parse_i64(value: &str) -> Result<i64> {
    let value = value.trim();
    if value.is_empty() {
        bail!("offset cannot be empty");
    }
    let (negative, unsigned) = match value.strip_prefix('-') {
        Some(unsigned) => (true, unsigned),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let magnitude = if let Some(hex) = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
    {
        i128::from_str_radix(hex, 16)?
    } else {
        unsigned.parse::<i128>()?
    };
    let signed = if negative { -magnitude } else { magnitude };
    i64::try_from(signed).map_err(|_| anyhow::anyhow!("offset does not fit in i64"))
}

pub fn format_address(address: u64) -> String {
    format!("0x{address:016X}")
}

pub fn checked_add(address: u64, offset: u64) -> Result<u64> {
    address
        .checked_add(offset)
        .ok_or_else(|| anyhow::anyhow!("address overflow"))
}

pub fn checked_add_signed(address: u64, offset: i64) -> Result<u64> {
    if offset >= 0 {
        address
            .checked_add(offset as u64)
            .ok_or_else(|| anyhow::anyhow!("address overflow"))
    } else {
        address
            .checked_sub(offset.unsigned_abs())
            .ok_or_else(|| anyhow::anyhow!("address underflow"))
    }
}

#[cfg(test)]
mod tests {
    use super::{checked_add_signed, format_address, parse_i64, parse_u64, parse_u128_as_u64};

    #[test]
    fn parses_decimal_and_hex_values() {
        assert_eq!(parse_u64("42").unwrap(), 42);
        assert_eq!(parse_u64("0x2a").unwrap(), 42);
        assert_eq!(parse_u128_as_u64("0x000000000000002a").unwrap(), 42);
    }

    #[test]
    fn rejects_addresses_outside_u64() {
        assert!(parse_u128_as_u64("0x10000000000000000").is_err());
    }

    #[test]
    fn formats_sixteen_digit_addresses() {
        assert_eq!(format_address(0x2a), "0x000000000000002A");
    }

    #[test]
    fn parses_signed_offsets() {
        assert_eq!(parse_i64("-0x2a").unwrap(), -42);
        assert_eq!(parse_i64("+42").unwrap(), 42);
        assert_eq!(checked_add_signed(0x100, -0x10).unwrap(), 0xf0);
    }
}
