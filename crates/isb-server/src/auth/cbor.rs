//! A minimal CBOR (RFC 8949) decoder for WebAuthn: attestation objects and
//! COSE keys. Definite lengths only (CTAP2 canonical encoding never uses
//! indefinite ones), nesting capped, every length checked against the input
//! before anything is allocated. Floats are skipped over, not interpreted.

/// A decoded item.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Uint(u64),
    /// A negative integer: the value is `-1 - n`.
    Nint(u64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Value>),
    Map(Vec<(Value, Value)>),
    Bool(bool),
    Null,
    Undefined,
    Float,
}

impl Value {
    /// This integer as an i64, if it fits.
    pub fn as_int(&self) -> Option<i64> {
        match *self {
            Value::Uint(n) => i64::try_from(n).ok(),
            Value::Nint(n) => i64::try_from(n).ok().map(|n| -1 - n),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// The value under integer key `k` in a map.
    pub fn get_int(&self, k: i64) -> Option<&Value> {
        match self {
            Value::Map(m) => m
                .iter()
                .find(|(key, _)| key.as_int() == Some(k))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value under text key `k` in a map.
    pub fn get_text(&self, k: &str) -> Option<&Value> {
        match self {
            Value::Map(m) => m
                .iter()
                .find(|(key, _)| key.as_text() == Some(k))
                .map(|(_, v)| v),
            _ => None,
        }
    }
}

const MAX_DEPTH: usize = 16;

/// Decode one item from the front of `input`; returns it and how many bytes
/// it took. Trailing bytes are the caller's business (authenticator data
/// carries extensions after the credential key).
pub fn decode(input: &[u8]) -> Result<(Value, usize), String> {
    let mut d = Decoder { b: input, at: 0 };
    let v = d.item(0)?;
    Ok((v, d.at))
}

/// Decode exactly one item spanning all of `input`.
pub fn decode_all(input: &[u8]) -> Result<Value, String> {
    let (v, n) = decode(input)?;
    if n != input.len() {
        return Err(format!(
            "{} trailing bytes after the CBOR item",
            input.len() - n
        ));
    }
    Ok(v)
}

struct Decoder<'a> {
    b: &'a [u8],
    at: usize,
}

impl Decoder<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let c = *self.b.get(self.at).ok_or("CBOR: unexpected end of input")?;
        self.at += 1;
        Ok(c)
    }

    fn take(&mut self, n: u64) -> Result<&[u8], String> {
        let n = usize::try_from(n).map_err(|_| "CBOR: length overflows")?;
        let end = self
            .at
            .checked_add(n)
            .filter(|e| *e <= self.b.len())
            .ok_or("CBOR: length runs past the end of input")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }

    /// The argument of an initial byte with additional info `ai`.
    fn arg(&mut self, ai: u8) -> Result<u64, String> {
        Ok(match ai {
            0..=23 => u64::from(ai),
            24 => u64::from(self.byte()?),
            25 => u64::from(u16::from_be_bytes(self.take(2)?.try_into().unwrap())),
            26 => u64::from(u32::from_be_bytes(self.take(4)?.try_into().unwrap())),
            27 => u64::from_be_bytes(self.take(8)?.try_into().unwrap()),
            31 => return Err("CBOR: indefinite lengths are not supported".into()),
            _ => return Err(format!("CBOR: reserved additional info {ai}")),
        })
    }

    /// A count of items that must each take at least one byte of input.
    fn count(&self, n: u64) -> Result<usize, String> {
        let left = (self.b.len() - self.at) as u64;
        if n > left {
            return Err("CBOR: item count runs past the end of input".into());
        }
        Ok(n as usize)
    }

    fn item(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err("CBOR: nested too deeply".into());
        }
        let ib = self.byte()?;
        let (major, ai) = (ib >> 5, ib & 0x1f);
        if major == 7 {
            return match ai {
                20 => Ok(Value::Bool(false)),
                21 => Ok(Value::Bool(true)),
                22 => Ok(Value::Null),
                23 => Ok(Value::Undefined),
                25 => self.take(2).map(|_| Value::Float),
                26 => self.take(4).map(|_| Value::Float),
                27 => self.take(8).map(|_| Value::Float),
                _ => Err(format!("CBOR: unsupported simple value {ai}")),
            };
        }
        let n = self.arg(ai)?;
        Ok(match major {
            0 => Value::Uint(n),
            1 => Value::Nint(n),
            2 => Value::Bytes(self.take(n)?.to_vec()),
            3 => Value::Text(
                String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "CBOR: text is not UTF-8")?,
            ),
            4 => {
                let n = self.count(n)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.item(depth + 1)?);
                }
                Value::Array(v)
            }
            5 => {
                let n = self.count(n.saturating_mul(2))? / 2;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    let k = self.item(depth + 1)?;
                    let val = self.item(depth + 1)?;
                    v.push((k, val));
                }
                Value::Map(v)
            }
            // A tag: keep the tagged item, drop the tag.
            6 => self.item(depth + 1)?,
            _ => unreachable!("major type is 3 bits"),
        })
    }
}

/// Encode (tests build attestation objects and COSE keys with it).
#[cfg(test)]
pub fn encode(v: &Value) -> Vec<u8> {
    fn head(out: &mut Vec<u8>, major: u8, n: u64) {
        let m = major << 5;
        match n {
            0..=23 => out.push(m | n as u8),
            24..=0xff => out.extend([m | 24, n as u8]),
            0x100..=0xffff => {
                out.push(m | 25);
                out.extend((n as u16).to_be_bytes());
            }
            0x1_0000..=0xffff_ffff => {
                out.push(m | 26);
                out.extend((n as u32).to_be_bytes());
            }
            _ => {
                out.push(m | 27);
                out.extend(n.to_be_bytes());
            }
        }
    }
    fn go(out: &mut Vec<u8>, v: &Value) {
        match v {
            Value::Uint(n) => head(out, 0, *n),
            Value::Nint(n) => head(out, 1, *n),
            Value::Bytes(b) => {
                head(out, 2, b.len() as u64);
                out.extend(b);
            }
            Value::Text(s) => {
                head(out, 3, s.len() as u64);
                out.extend(s.as_bytes());
            }
            Value::Array(a) => {
                head(out, 4, a.len() as u64);
                a.iter().for_each(|x| go(out, x));
            }
            Value::Map(m) => {
                head(out, 5, m.len() as u64);
                for (k, x) in m {
                    go(out, k);
                    go(out, x);
                }
            }
            Value::Bool(b) => out.push(if *b { 0xf5 } else { 0xf4 }),
            Value::Null => out.push(0xf6),
            Value::Undefined => out.push(0xf7),
            Value::Float => out.extend([0xf9, 0, 0]),
        }
    }
    let mut out = Vec::new();
    go(&mut out, v);
    out
}

/// An integer as a CBOR value (tests).
#[cfg(test)]
pub fn int(i: i64) -> Value {
    if i >= 0 {
        Value::Uint(i as u64)
    } else {
        Value::Nint((-1 - i) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_reports_length() {
        let v = Value::Map(vec![
            (int(1), int(2)),
            (int(3), int(-7)),
            (int(-1), int(1)),
            (int(-2), Value::Bytes(vec![7; 32])),
            (Value::Text("fmt".into()), Value::Text("none".into())),
            (
                Value::Text("a".into()),
                Value::Array(vec![Value::Bool(true), Value::Null, int(1000), int(70000)]),
            ),
        ]);
        let mut b = encode(&v);
        let n = b.len();
        b.extend([0xa0, 0xff]);
        let (d, used) = decode(&b).unwrap();
        assert_eq!(used, n);
        assert_eq!(d, v);
        assert_eq!(d.get_int(3).and_then(Value::as_int), Some(-7));
        assert_eq!(d.get_text("fmt").and_then(Value::as_text), Some("none"));
        assert!(decode_all(&b).is_err());
    }

    #[test]
    fn known_encodings() {
        // RFC 8949 appendix A.
        assert_eq!(decode_all(&[0x19, 0x03, 0xe8]).unwrap(), Value::Uint(1000));
        assert_eq!(decode_all(&[0x38, 0x63]).unwrap().as_int(), Some(-100));
        assert_eq!(
            decode_all(&[0x82, 0x01, 0x82, 0x02, 0x03]).unwrap(),
            Value::Array(vec![int(1), Value::Array(vec![int(2), int(3)])])
        );
        // A tagged item decodes to the item.
        assert_eq!(decode_all(&[0xc1, 0x01]).unwrap(), Value::Uint(1));
    }

    #[test]
    fn refuses_hostile_input() {
        // Indefinite length, a byte string longer than the input, a huge
        // array count, deep nesting, truncation, bad UTF-8.
        assert!(decode(&[0x5f]).is_err());
        assert!(decode(&[0x5a, 0xff, 0xff, 0xff, 0xff, 0x00]).is_err());
        assert!(decode(&[0x9b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).is_err());
        assert!(decode(&[0xbb, 0x80, 0, 0, 0, 0, 0, 0, 0]).is_err());
        assert!(decode(&[0x81; 40]).is_err());
        assert!(decode(&[0x19, 0x03]).is_err());
        assert!(decode(&[0x62, 0xff, 0xfe]).is_err());
        assert!(decode(&[]).is_err());
    }
}
