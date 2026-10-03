use std::env;

const INVALID_PORT: &str = "PORT must be a decimal TCP port from 1 to 65535";
pub const DEFAULT_PORT: u16 = 4730;

pub fn from_env() -> Result<u16, &'static str> {
    match env::var("PORT") {
        Ok(value) => parse(Some(&value)),
        Err(env::VarError::NotPresent) => parse(None),
        Err(env::VarError::NotUnicode(_)) => Err(INVALID_PORT),
    }
}

fn parse(value: Option<&str>) -> Result<u16, &'static str> {
    let Some(value) = value else {
        return Ok(DEFAULT_PORT);
    };
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(INVALID_PORT);
    }
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or(INVALID_PORT)
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_PORT, parse};

    #[test]
    fn absent_port_uses_the_default() {
        assert_eq!(parse(None), Ok(DEFAULT_PORT));
    }

    #[test]
    fn accepts_decimal_ports_in_range() {
        for (value, port) in [("1", 1), ("3100", 3100), ("65535", 65535), ("04730", 4730)] {
            assert_eq!(parse(Some(value)), Ok(port));
        }
    }

    #[test]
    fn rejects_invalid_and_out_of_range_ports() {
        for value in [
            "",
            "0",
            "65536",
            "abc",
            "-1",
            "+4730",
            " 4730",
            "4730 ",
            "127.0.0.1:4730",
        ] {
            assert!(parse(Some(value)).is_err(), "accepted {value:?}");
        }
    }
}
