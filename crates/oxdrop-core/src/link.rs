use std::fmt;
use std::net::SocketAddr;

use crate::TransferSecret;

/// The address that gives whoever holds it access to one open Transfer,
/// e.g. `http://192.168.1.20:48230/t/<secret>/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferLink {
    pub address: SocketAddr,
    pub secret: TransferSecret,
}

impl fmt::Display for TransferLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "http://{}/t/{}/", self.address, self.secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_as_http_url_with_trailing_slash() {
        let secret = TransferSecret::generate();
        let link = TransferLink {
            address: "192.168.1.20:48230".parse().unwrap(),
            secret: secret.clone(),
        };
        assert_eq!(
            link.to_string(),
            format!("http://192.168.1.20:48230/t/{secret}/")
        );
    }
}
