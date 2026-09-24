use anyhow::{Context, Result, bail};

#[derive(Debug, Clone)]
pub struct BytePattern {
    tokens: Vec<Option<u8>>,
    source: String,
}

impl BytePattern {
    pub fn parse(input: &str) -> Result<Self> {
        let normalized = input.replace(',', " ");
        let mut tokens = Vec::new();
        for token in normalized.split_whitespace() {
            if token == "?" || token == "??" {
                tokens.push(None);
                continue;
            }
            if token.len() != 2 {
                bail!("pattern token '{token}' must contain two hex digits or ??");
            }
            let value = u8::from_str_radix(token, 16)
                .with_context(|| format!("invalid pattern token '{token}'"))?;
            tokens.push(Some(value));
        }
        if tokens.is_empty() {
            bail!("pattern cannot be empty");
        }
        Ok(Self {
            tokens,
            source: input.to_owned(),
        })
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn matches_at(&self, bytes: &[u8], offset: usize) -> bool {
        if offset + self.tokens.len() > bytes.len() {
            return false;
        }
        self.tokens
            .iter()
            .enumerate()
            .all(|(index, token)| token.is_none_or(|expected| bytes[offset + index] == expected))
    }

    pub fn find_all(&self, bytes: &[u8], base_address: u64, limit: usize) -> Vec<u64> {
        if self.is_empty() || limit == 0 || bytes.len() < self.len() {
            return Vec::new();
        }
        let mut matches = Vec::new();
        let last = bytes.len() - self.len();
        for offset in 0..=last {
            if self.matches_at(bytes, offset) {
                matches.push(base_address.saturating_add(offset as u64));
                if matches.len() >= limit {
                    break;
                }
            }
        }
        matches
    }
}

#[cfg(test)]
mod tests {
    use super::BytePattern;

    #[test]
    fn matches_wildcards_and_returns_addresses() {
        let pattern = BytePattern::parse("48 8B ?? 05").unwrap();
        let bytes = [0x48, 0x8B, 0x11, 0x05];
        assert_eq!(pattern.find_all(&bytes, 0x1000, 10), vec![0x1000]);
    }

    #[test]
    fn accepts_comma_separated_patterns() {
        let pattern = BytePattern::parse("90,90,??").unwrap();
        assert_eq!(pattern.len(), 3);
        assert!(pattern.matches_at(&[0x90, 0x90, 0x00], 0));
    }

    #[test]
    fn rejects_invalid_pattern_tokens() {
        assert!(BytePattern::parse("48 0").is_err());
        assert!(BytePattern::parse("").is_err());
    }
}
