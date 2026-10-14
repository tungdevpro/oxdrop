# Transfers use plain HTTP, protected only by the Transfer Link

Items are served over unencrypted HTTP on the local network. Access requires the unguessable secret in the Transfer Link, which dies when the Transfer ends. We accept that anyone passively capturing traffic on the same network can read transferred files.

## Considered Options

- **Self-signed HTTPS**: encrypts, but every Receiver hits a full-screen "connection is not private" warning, which is unacceptable UX and trains users to click through warnings.
- **Real certificates via per-user public hostnames resolving to LAN IPs**: requires us to operate DNS and certificate infrastructure (contradicts ADR-0001) and is broken by common router DNS-rebinding protection.
- **In-browser decryption**: WebCrypto and Service Workers require a secure context, so it cannot work over plain HTTP for multi-GB files.

## Consequences

The Sender tells users to use Oxdrop only on networks they trust. Revisit if browsers gain a practical way to trust a local endpoint.
