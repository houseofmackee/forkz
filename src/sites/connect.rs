//! Connecting to a saved site, for the app and for the command line. The
//! questions on the way (host keys, passwords, AWS sign-ins) go to an
//! [`Ask`]: dialogs in the app, the terminal on the command line.

use std::path::PathBuf;
use std::time::Duration;

use crate::vfs::{self, FsRef};

/// Answers the questions of connecting.
pub trait Ask: vfs::sftp::Prompter + vfs::aws::SignIn {
    /// Asks again later, when an AWS sign-in is renewed (with no offer to
    /// save a password).
    fn for_renewal(&self) -> Box<dyn Ask>;
}

/// What to mount for a share site: the server and the share (the first
/// part of the remote folder).
pub fn share_params(site: &crate::sites::Site, password: Option<String>) -> vfs::mount::ShareParams {
    let scheme = site.protocol.scheme();
    let share = site.remote_dir.clone().unwrap_or_default();
    // NFS exports are paths; SMB and AFP mount the share, not a folder in it.
    let share =
        if scheme == "nfs" { share } else { share.trim_matches('/').split('/').next().unwrap_or("").to_string() };
    vfs::mount::ShareParams {
        scheme,
        host: site.host.clone(),
        port: (site.port != site.protocol.default_port()).then_some(site.port),
        user: site.user.clone(),
        password,
        share,
    }
}

/// Connects to a site with its protocol (off the main thread). Returns the
/// file system and the password or key that worked.
pub fn open(
    site: &crate::sites::Site,
    password: Option<String>,
    timeout: Duration,
    extra_keys: Vec<PathBuf>,
    prompter: &dyn Ask,
) -> std::io::Result<(FsRef, Option<String>)> {
    use crate::sites::Protocol;
    let user = if site.user.is_empty() && !site.protocol.is_object_storage() {
        std::env::var("USER").unwrap_or_default()
    } else {
        site.user.clone()
    };
    match site.protocol {
        Protocol::Sftp => {
            let ssh = vfs::ssh_config::SshConfig::load().resolve(&site.host);
            let params = site.params(&ssh, password, timeout, extra_keys);
            tracing::debug!("connecting with {params:?} (from ~/.ssh/config: {ssh:?})");
            let fs = vfs::sftp::connect(params, prompter)?;
            let secret = fs.secret().map(str::to_string);
            Ok((fs, secret))
        }
        Protocol::Ftp | Protocol::Ftps | Protocol::FtpPlain => {
            let security = match site.protocol {
                Protocol::Ftps => vfs::ftp::Security::Tls,
                Protocol::FtpPlain => vfs::ftp::Security::Plain,
                _ => vfs::ftp::Security::TlsIfAvailable,
            };
            // FTP servers expect "anonymous" when there is no account.
            let user = if site.user.is_empty() { "anonymous".to_string() } else { user };
            let params =
                vfs::ftp::FtpParams { host: site.host.clone(), port: site.port, user, password, security, timeout };
            let fs = vfs::ftp::connect(params, prompter)?;
            let secret = fs.secret().map(str::to_string);
            Ok((fs, secret))
        }
        Protocol::Webdav | Protocol::Webdavs => {
            let root = site.remote_dir.clone().unwrap_or_else(|| "/".to_string());
            let params = vfs::webdav::DavParams {
                host: site.host.clone(),
                port: site.port,
                secure: site.protocol == Protocol::Webdavs,
                user: site.user.clone(),
                password,
                root,
                timeout,
            };
            let fs = vfs::webdav::connect(params, prompter)?;
            let secret = fs.secret().map(str::to_string);
            Ok((fs, secret))
        }
        Protocol::Smb | Protocol::Afp | Protocol::Nfs => {
            let params = share_params(site, password);
            let mountpoint = vfs::mount::mount(&params)?;
            let fs: FsRef = vfs::mount::MountFs::new(mountpoint, &params);
            Ok((fs, params.password))
        }
        Protocol::S3 | Protocol::S3Http if site.aws_profile.is_some() => {
            let profile = site.aws_profile.clone().unwrap_or_default();
            let bucket = first_folder(site.remote_dir.as_deref());
            let keys = vfs::aws::credentials(&profile, prompter, prompter)?;
            // A renewal may ask again (an MFA code, a sign-in), from the
            // thread that needs the keys.
            let name = profile.clone();
            let renewer = prompter.for_renewal();
            let renew: vfs::s3::Renew =
                Box::new(move || vfs::aws::credentials(&name, renewer.as_ref(), renewer.as_ref()));
            let host = if site.host.trim().is_empty() { "s3.amazonaws.com".to_string() } else { site.host.clone() };
            // The site's region, else the bucket's (on AWS), else the
            // profile's.
            let set = site.region.as_deref().map(str::trim).filter(|r| !r.is_empty()).map(str::to_string);
            let aws = host.eq_ignore_ascii_case("s3.amazonaws.com");
            let found = if set.is_none() && aws {
                bucket.as_deref().and_then(|b| vfs::s3::bucket_region(b, timeout))
            } else {
                None
            };
            let region = set.or(found).or_else(|| vfs::aws::region(&profile)).unwrap_or_else(|| site.s3_region());
            let host = if aws { format!("s3.{region}.amazonaws.com") } else { host };
            let params = vfs::s3::S3Params {
                host,
                port: site.port,
                secure: site.protocol == Protocol::S3,
                access_key: profile,
                secret_key: None,
                region,
                timeout,
                check: bucket,
                session: Some(vfs::s3::Session::new(keys, renew)),
            };
            let fs: FsRef = vfs::s3::connect(params, prompter)?;
            Ok((fs, None))
        }
        Protocol::S3 | Protocol::S3Http => {
            let bucket = first_folder(site.remote_dir.as_deref());
            // AWS's general server with no region: the bucket's region, and
            // its server there (requests are signed for a region).
            let general = site.host.eq_ignore_ascii_case("s3.amazonaws.com")
                && site.region.as_deref().is_none_or(|r| r.trim().is_empty());
            let found = bucket.as_deref().filter(|_| general).and_then(|b| vfs::s3::bucket_region(b, timeout));
            let (host, region) = match found {
                Some(region) => (format!("s3.{region}.amazonaws.com"), region),
                None => (site.host.clone(), site.s3_region()),
            };
            let params = vfs::s3::S3Params {
                host,
                port: site.port,
                secure: site.protocol == Protocol::S3,
                access_key: site.user.clone(),
                secret_key: password,
                region,
                timeout,
                check: bucket,
                session: None,
            };
            let fs = vfs::s3::connect(params, prompter)?;
            let secret = fs.secret().map(str::to_string);
            Ok((fs, secret))
        }
        // Google's S3-compatible API, with HMAC keys (region "auto").
        Protocol::Gcs => {
            let host =
                if site.host.trim().is_empty() { "storage.googleapis.com".to_string() } else { site.host.clone() };
            let params = vfs::s3::S3Params {
                host,
                port: site.port,
                secure: true,
                access_key: site.user.clone(),
                secret_key: password,
                region: site.region.clone().filter(|r| !r.trim().is_empty()).unwrap_or_else(|| "auto".into()),
                timeout,
                check: first_folder(site.remote_dir.as_deref()),
                session: None,
            };
            let fs = vfs::s3::connect(params, prompter)?;
            let secret = fs.secret().map(str::to_string);
            Ok((fs, secret))
        }
        Protocol::Dropbox => {
            let saved = crate::sites::SiteStore::load().get(&site.id).is_some();
            let id = site.id.clone();
            let save_token: Option<vfs::dropbox::SaveToken> = saved.then(|| {
                std::sync::Arc::new(move |token: &str| crate::sites::keychain::save_password(&id, token)) as _
            });
            let params = vfs::dropbox::DropboxParams {
                refresh_token: password,
                save_token,
                timeout,
                service: Default::default(),
                browser: true,
            };
            let fs = vfs::dropbox::connect(params)?;
            Ok((fs, None))
        }
        Protocol::OneDrive => {
            // Microsoft replaces the sign-in each time it is renewed: a
            // saved site keeps the latest one in the Keychain.
            let saved = crate::sites::SiteStore::load().get(&site.id).is_some();
            let id = site.id.clone();
            let save_token: Option<vfs::onedrive::SaveToken> = saved.then(|| {
                std::sync::Arc::new(move |token: &str| crate::sites::keychain::save_password(&id, token)) as _
            });
            let params = vfs::onedrive::OneDriveParams {
                refresh_token: password,
                save_token,
                timeout,
                service: Default::default(),
                browser: true,
            };
            let fs = vfs::onedrive::connect(params)?;
            Ok((fs, None))
        }
        Protocol::GoogleDrive => {
            let saved = crate::sites::SiteStore::load().get(&site.id).is_some();
            let id = site.id.clone();
            let save_token: Option<vfs::gdrive::SaveToken> = saved.then(|| {
                std::sync::Arc::new(move |token: &str| crate::sites::keychain::save_password(&id, token)) as _
            });
            let params = vfs::gdrive::DriveParams {
                refresh_token: password,
                save_token,
                timeout,
                service: Default::default(),
                browser: true,
            };
            let fs = vfs::gdrive::connect(params)?;
            Ok((fs, None))
        }
        Protocol::Box => {
            // Box replaces the sign-in each time it is renewed: a saved
            // site keeps the latest one in the Keychain.
            let saved = crate::sites::SiteStore::load().get(&site.id).is_some();
            let id = site.id.clone();
            let save_token: Option<vfs::boxcom::SaveToken> = saved.then(|| {
                std::sync::Arc::new(move |token: &str| crate::sites::keychain::save_password(&id, token)) as _
            });
            let params = vfs::boxcom::BoxParams {
                refresh_token: password,
                save_token,
                timeout,
                service: Default::default(),
                browser: true,
            };
            let fs = vfs::boxcom::connect(params)?;
            Ok((fs, None))
        }
        Protocol::Azure => {
            let host = Protocol::Azure
                .default_host(&site.user)
                .filter(|_| site.host.trim().is_empty())
                .unwrap_or_else(|| site.host.clone());
            // Azurite (the emulator) answers over plain HTTP.
            let local = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok();
            let params = vfs::azure::AzureParams {
                host,
                port: site.port,
                secure: !local,
                account: site.user.clone(),
                key: password,
                timeout,
            };
            let fs = vfs::azure::connect(params, prompter)?;
            let key = fs.key().map(str::to_string);
            Ok((fs, key))
        }
    }
}

/// The first folder of a remote path ("/bucket/folder" gives "bucket").
pub fn first_folder(path: Option<&str>) -> Option<String> {
    path?.trim_matches('/').split('/').next().filter(|b| !b.is_empty()).map(str::to_string)
}
