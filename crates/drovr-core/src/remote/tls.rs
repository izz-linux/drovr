//! HTTPS for the phone page. A TLS listener on `remote.bind` decrypts each
//! connection and passes the plain HTTP to the page's server, which then
//! listens only on this Mac's loopback address.
//!
//! The certificate is read again when its files change, so a renewal (Let's
//! Encrypt's every 60 days or so) needs no restart: new connections get the
//! new certificate.

use anyhow::{anyhow, bail, Context, Result};
use rustls::{ServerConfig, ServerConnection};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// More than a few phones and browser tabs will ever open.
const MAX_CONNECTIONS: usize = 64;
/// A connection with no traffic either way for this long is closed.
const IDLE: Duration = Duration::from_secs(120);
/// How long each side is waited on before checking the other.
const TICK: Duration = Duration::from_millis(15);

/// Read a certificate chain and its key into a TLS configuration.
pub fn load(cert: &Path, key: &Path) -> Result<Arc<ServerConfig>> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .map_err(|e| anyhow!("reading {}: {e}", cert.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow!("reading {}: {e}", cert.display()))?;
    if certs.is_empty() {
        bail!("{} has no certificates in it", cert.display());
    }
    // PKCS#8, RSA (PKCS#1) and EC (SEC1) keys, whichever the ACME client wrote.
    let key_der = PrivateKeyDer::from_pem_file(key).map_err(|e| {
        anyhow!(
            "reading the private key {}: {e} (drovr needs to be able to read it)",
            key.display()
        )
    })?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key_der)
        .with_context(|| format!("{} and {} don't go together", cert.display(), key.display()))?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

fn modified(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

struct Loaded {
    stamp: (Option<SystemTime>, Option<SystemTime>),
    config: Arc<ServerConfig>,
}

pub struct Acceptor {
    cert: PathBuf,
    key: PathBuf,
    loaded: Mutex<Loaded>,
    open: AtomicUsize,
}

impl Acceptor {
    /// Fails if the certificate or key can't be used, so a mistake shows
    /// when drovr starts rather than on your phone.
    pub fn new(cert: PathBuf, key: PathBuf) -> Result<Acceptor> {
        let config = load(&cert, &key)?;
        let stamp = (modified(&cert), modified(&key));
        Ok(Acceptor {
            cert,
            key,
            loaded: Mutex::new(Loaded { stamp, config }),
            open: AtomicUsize::new(0),
        })
    }

    /// The configuration for a new connection, read again if either file
    /// changed. A renewal that's only half written (or broken) keeps the
    /// certificate drovr already had until the files make sense again.
    fn config(&self) -> Arc<ServerConfig> {
        let mut loaded = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
        let stamp = (modified(&self.cert), modified(&self.key));
        if stamp != loaded.stamp {
            match load(&self.cert, &self.key) {
                Ok(config) => {
                    loaded.config = config;
                    loaded.stamp = stamp;
                    eprintln!("drovr: phone page: loaded the renewed certificate");
                }
                Err(e) => eprintln!("drovr: phone page: keeping the old certificate: {e:#}"),
            }
        }
        loaded.config.clone()
    }

    /// Accept connections on `listener` and pass each one, decrypted, to
    /// `backend`. Runs until the listener fails.
    pub fn run(self: Arc<Self>, listener: TcpListener, backend: SocketAddr) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if self.open.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                let _ = stream.shutdown(Shutdown::Both);
                continue;
            }
            self.open.fetch_add(1, Ordering::SeqCst);
            let config = self.config();
            let me = self.clone();
            std::thread::spawn(move || {
                let _ = relay(stream, config, backend);
                me.open.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }
}

fn waiting(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

/// One connection: the TLS side to the phone, plain HTTP to the server.
fn relay(client: TcpStream, config: Arc<ServerConfig>, backend: SocketAddr) -> Result<()> {
    let mut tls = ServerConnection::new(config)?;
    let mut client = client;
    client.set_nodelay(true)?;
    client.set_read_timeout(Some(TICK))?;
    let mut server = TcpStream::connect(backend)?;
    server.set_nodelay(true)?;
    server.set_read_timeout(Some(TICK))?;
    let mut buf = vec![0u8; 16 * 1024];
    let mut last = Instant::now();
    loop {
        if last.elapsed() > IDLE {
            break;
        }
        // From the phone: TLS records in, plain HTTP out to the server.
        let mut closed = false;
        match tls.read_tls(&mut client) {
            Ok(0) => closed = true,
            Ok(_) => {
                last = Instant::now();
                if let Err(e) = tls.process_new_packets() {
                    // Tell the phone why (a TLS alert), then give up.
                    let _ = tls.write_tls(&mut client);
                    return Err(e.into());
                }
            }
            Err(e) if waiting(&e) => {}
            Err(e) => return Err(e.into()),
        }
        loop {
            match tls.reader().read(&mut buf) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(n) => server.write_all(&buf[..n])?,
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        // From the server: plain HTTP in, TLS records out to the phone.
        match server.read(&mut buf) {
            Ok(0) => {
                tls.send_close_notify();
                closed = true;
            }
            Ok(n) => {
                last = Instant::now();
                tls.writer().write_all(&buf[..n])?;
            }
            Err(e) if waiting(&e) => {}
            Err(e) => return Err(e.into()),
        }
        while tls.wants_write() {
            tls.write_tls(&mut client)?;
        }
        if closed {
            break;
        }
    }
    let _ = server.shutdown(Shutdown::Both);
    let _ = client.shutdown(Shutdown::Both);
    Ok(())
}
