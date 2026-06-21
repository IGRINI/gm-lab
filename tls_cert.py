"""Self-signed TLS cert so GM-Lab can serve HTTPS on the LAN.

Browsers expose the microphone (getUserMedia / MediaRecorder) ONLY in a secure
context — https:// or localhost. So voice dictation from a phone/tablet over the
LAN (http://192.168.x.x:port) is blocked and the mic button won't even show.
Serving over https fixes that. This generates a long-lived self-signed cert
(cached under <project>/.tls) whose SAN covers localhost + the machine's LAN IPs.
The browser will still warn once for a self-signed cert — accept it to proceed.
"""
from __future__ import annotations

import datetime
import ipaddress
import socket
from pathlib import Path


def local_ips() -> list[str]:
    ips: set[str] = {"127.0.0.1"}
    try:
        for info in socket.getaddrinfo(socket.gethostname(), None):
            ips.add(info[4][0])
    except Exception:
        pass
    try:  # primary outbound IPv4
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("8.8.8.8", 80))
        ips.add(s.getsockname()[0])
        s.close()
    except Exception:
        pass
    return sorted(ips)


def lan_ipv4() -> list[str]:
    out = []
    for ip in local_ips():
        try:
            addr = ipaddress.ip_address(ip)
        except ValueError:
            continue
        if addr.version == 4 and not addr.is_loopback:
            out.append(ip)
    return out


def ensure_self_signed(cert_dir: Path) -> tuple[str, str]:
    cert_dir.mkdir(parents=True, exist_ok=True)
    cert_path = cert_dir / "gmlab-cert.pem"
    key_path = cert_dir / "gmlab-key.pem"
    if cert_path.exists() and key_path.exists():
        return str(cert_path), str(key_path)

    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.x509.oid import NameOID

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "gm-lab.local")])

    san: list = [x509.DNSName("localhost")]
    for ip in local_ips():
        try:
            san.append(x509.IPAddress(ipaddress.ip_address(ip)))
        except ValueError:
            san.append(x509.DNSName(ip))

    now = datetime.datetime.now(datetime.timezone.utc)
    cert = (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(name)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(days=1))
        .not_valid_after(now + datetime.timedelta(days=3650))
        .add_extension(x509.SubjectAlternativeName(san), critical=False)
        .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
        .sign(key, hashes.SHA256())
    )

    key_path.write_bytes(
        key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.TraditionalOpenSSL,
            serialization.NoEncryption(),
        )
    )
    cert_path.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    return str(cert_path), str(key_path)
