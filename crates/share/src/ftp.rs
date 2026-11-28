//! FTP and FTPS (explicit AUTH TLS, or implicit): passive binary uploads to a
//! `.part` file renamed into place, MKD level by level, DELE for delete.

use std::path::Path;
use std::time::Duration;

use heronite_i18n::tr;
use suppaftp::types::FileType;
use suppaftp::{FtpError, FtpStream, RustlsConnector, RustlsFtpStream};

use crate::config::{unseal, Ftp, FtpTls, Target};
use crate::error::{Result, ShareError};
use crate::history::Record;
use crate::progress::{Continue, CountingReader, Progress};
use crate::Shared;

/// The sync stream is generic over its TLS connector flavour; both arms end
/// up as the same type here, so the connector enum abstracts them.
pub enum Stream {
    Plain(FtpStream),
    Rustls(RustlsFtpStream),
}

macro_rules! with {
    ($stream:expr, $s:ident, $body:expr) => {
        match $stream {
            Stream::Plain($s) => $body,
            Stream::Rustls($s) => $body,
        }
    };
}

fn map_err(e: FtpError) -> ShareError {
    match &e {
        FtpError::UnexpectedResponse(response) => {
            let text = String::from_utf8_lossy(&response.body).trim().to_string();
            let code = format!("{:?}", response.status);
            let digit = code.chars().find(|c| c.is_ascii_digit()).unwrap_or('0');
            match digit {
                '5' if text.contains("530") || code.contains("530") => ShareError::AuthFailed(text),
                _ if text.contains("552") || text.contains("553") => ShareError::TooLarge(text),
                _ if text.contains("550") => ShareError::NotFound(text),
                '5' => ShareError::Server { status: 500, body: text },
                '4' => ShareError::Forbidden(text),
                _ => ShareError::Server { status: 0, body: text },
            }
        }
        FtpError::SecureError(t) => ShareError::Tls(t.clone()),
        FtpError::ConnectionError(io) => ShareError::network(io),
        other => ShareError::network(other),
    }
}

fn login_error(e: FtpError) -> ShareError {
    if matches!(&e, FtpError::UnexpectedResponse(_)) {
        return ShareError::AuthFailed(tr!("the server refused the login").into());
    }
    map_err(e)
}

fn connect(target: &Target, f: &Ftp) -> Result<Stream> {
    let addr = format!("{}:{}", f.host, f.port);
    let timeout = Duration::from_secs(15);
    let socket = std::net::ToSocketAddrs::to_socket_addrs(&addr.as_str())
        .map_err(ShareError::io)?
        .next()
        .ok_or_else(|| ShareError::network(format!("{addr} does not resolve")))?;
    match f.tls {
        FtpTls::None => {
            let stream = FtpStream::connect_timeout(socket, timeout).map_err(map_err)?;
            Ok(Stream::Plain(stream))
        }
        FtpTls::Explicit => {
            let stream = RustlsFtpStream::connect_timeout(socket, timeout).map_err(map_err)?;
            let connector = RustlsConnector::from(target.tls.rustls_config()?);
            let stream = stream.into_secure(connector, &f.host).map_err(map_err)?;
            Ok(Stream::Rustls(stream))
        }
        FtpTls::Implicit => {
            let connector = RustlsConnector::from(target.tls.rustls_config()?);
            let stream = RustlsFtpStream::connect_secure_implicit(socket, connector, &f.host).map_err(map_err)?;
            Ok(Stream::Rustls(stream))
        }
    }
}

fn finish_login(stream: &mut Stream, f: &Ftp) -> Result<()> {
    let (user, password) = if f.user.trim().is_empty() {
        ("anonymous".to_string(), "heronite@".to_string())
    } else {
        (f.user.clone(), if f.sealed_password.is_empty() { String::new() } else { unseal(&f.sealed_password)? })
    };
    with!(stream, s, s.login(&user, &password).map_err(login_error))?;
    with!(stream, s, s.set_mode(suppaftp::types::Mode::Passive));
    with!(stream, s, s.transfer_type(FileType::Binary).map_err(map_err))?;
    Ok(())
}

/// MKD level by level, "exists" fine, then CWD into the deepest one.
fn cwd_into(stream: &mut Stream, folder: &str) -> Result<()> {
    for segment in folder.split('/').filter(|s| !s.is_empty()) {
        let r = with!(stream, s, s.mkdir(segment));
        match r {
            Ok(()) | Err(FtpError::BadResponse) => {}
            Err(e) => {
                // 550/521/522 mean "exists" on many servers; try to enter and
                // let the CWD decide.
                let text = e.to_string();
                if !text.contains("550") && !text.contains("521") {
                    return Err(map_err(e));
                }
            }
        }
        with!(stream, s, s.cwd(segment).map_err(map_err))?;
    }
    Ok(())
}

fn part_name(remote: &str) -> (String, String) {
    let name = remote.rsplit('/').next().unwrap_or(remote);
    (name.to_string(), format!(".{name}.part"))
}

pub fn upload(
    target: &Target,
    f: &Ftp,
    file: &Path,
    remote: &str,
    size: u64,
    progress: &mut dyn FnMut(Progress) -> Continue,
) -> Result<Shared> {
    // `remote` arrives with the target's folder prefix already joined.
    let folder = remote.rsplit_once('/').map(|(d, _)| d).unwrap_or("").trim_matches('/').to_string();
    let (name, part) = part_name(remote);
    let mut stream = connect(target, f)?;
    finish_login(&mut stream, f)?;
    if !folder.is_empty() {
        cwd_into(&mut stream, &folder)?;
    }
    let mut file_read = std::fs::File::open(file).map_err(ShareError::io)?;
    let outcome = {
        let mut counting = CountingReader::new(&mut file_read, size, &mut *progress);
        with!(&mut stream, s, s.put_file(&part, &mut counting).map_err(map_err))
    };
    match outcome {
        Ok(_) => {}
        Err(e) => {
            let _ = with!(&mut stream, s, s.rm(&part));
            if matches!(&e, ShareError::Network(t) if t.contains("Interrupted") || t.contains("cancelled")) {
                return Err(ShareError::Cancelled);
            }
            return Err(e);
        }
    }
    if progress(Progress { sent: size, total: size }).cancelled() {
        let _ = with!(&mut stream, s, s.rm(&part));
        return Err(ShareError::Cancelled);
    }
    // RNFR/RNTO into place; some servers refuse, then the direct name it is.
    let renamed = with!(&mut stream, s, s.rename(&part, &name));
    let final_name = match renamed {
        Ok(()) => name.clone(),
        Err(_) => {
            // Clean up the part and STOR the file under its final name.
            let _ = with!(&mut stream, s, s.rm(&part));
            let mut file_read = std::fs::File::open(file).map_err(ShareError::io)?;
            with!(&mut stream, s, s.put_file(&name, &mut file_read).map_err(map_err))?;
            name.clone()
        }
    };
    let _ = with!(&mut stream, s, s.quit());
    let _ = final_name;
    Ok(Shared { url: target.link_of(remote), remote: remote.to_string(), delete: Some(remote.to_string()) })
}

pub fn delete(target: &Target, f: &Ftp, record: &Record) -> Result<()> {
    let mut stream = connect(target, f)?;
    finish_login(&mut stream, f)?;
    let remote = record.delete.clone().unwrap_or_else(|| record.remote.clone());
    let (dir, name) = match remote.rsplit_once('/') {
        Some((d, n)) => (d, n),
        None => ("", remote.as_str()),
    };
    if !dir.is_empty() {
        with!(&mut stream, s, s.cwd(dir).map_err(map_err))?;
    }
    // A missing file is fine: best effort.
    let _ = with!(&mut stream, s, s.rm(name));
    let _ = with!(&mut stream, s, s.quit());
    Ok(())
}

pub fn check(target: &Target, f: &Ftp) -> Result<crate::CheckReport> {
    let mut stream = connect(target, f)?;
    finish_login(&mut stream, f)?;
    let mut steps = vec![tr!("Connected and logged in").into()];
    let folder = target.folder.trim_matches('/');
    if !folder.is_empty() {
        cwd_into(&mut stream, folder)?;
    }
    let probe = "heronite-probe.txt";
    let mut data = std::io::Cursor::new(b"probe".to_vec());
    with!(&mut stream, s, s.put_file(probe, &mut data).map_err(map_err))?;
    steps.push(tr!("Upload works").into());
    let back = with!(&mut stream, s, s.retr_as_buffer(probe).map_err(map_err))?;
    if back.get_ref() != b"probe" {
        return Err(ShareError::Server { status: 0, body: tr!("the probe reads back wrong").into() });
    }
    steps.push(tr!("Reading back works").into());
    with!(&mut stream, s, s.rm(probe).map_err(map_err))?;
    steps.push(tr!("Delete works").into());
    let _ = with!(&mut stream, s, s.quit());
    Ok(crate::CheckReport { steps })
}
