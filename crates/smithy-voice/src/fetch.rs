//! Getting the model onto disk: download, check, unpack.
//!
//! The archive is streamed to a `.part` file while it is hashed, compared with
//! the SHA-256 published beside it, and only then unpacked — into a temporary
//! directory that is renamed into place at the end, so an interrupted download
//! or unpack leaves nothing that looks like a model.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::Path;

use crate::model::ModelConfig;
use crate::voice_debug;

/// Make sure the model is on disk, downloading it if it is not.
pub fn ensure(config: &ModelConfig) -> Result<()> {
    if config.is_cached() {
        return Ok(());
    }
    let cache = Path::new(&config.cache_dir);
    fs::create_dir_all(cache)
        .with_context(|| format!("could not create the model cache at {}", cache.display()))?;
    let archive = cache.join(format!("{}.tar.bz2", config.name));
    let part = cache.join(format!("{}.tar.bz2.part", config.name));

    voice_debug!(
        "downloading {} ({} MB) …",
        config.name,
        config.size / 1_000_000
    );
    let response = ureq::get(&config.url)
        .call()
        .with_context(|| format!("could not download the speech model from {}", config.url))?;
    let mut body = response.into_reader();
    let written = copy_hashed(&mut body, &part)?;
    check(&part, written, config)?;
    fs::rename(&part, &archive).context("could not keep the downloaded archive")?;

    voice_debug!("unpacking …");
    unpack(&archive, cache, &config.name)?;
    let _ = fs::remove_file(&archive);
    if !config.is_cached() {
        bail!(
            "the speech model archive unpacked, but {} is missing files the recognizer needs",
            config.dir().display()
        );
    }
    voice_debug!("model ready at {}", config.dir().display());
    Ok(())
}

/// Stream `from` into `to`, returning the bytes written and their SHA-256.
fn copy_hashed(from: &mut dyn Read, to: &Path) -> Result<(u64, String)> {
    let mut file = File::create(to).with_context(|| format!("could not write {}", to.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = from
            .read(&mut buf)
            .context("the download was interrupted")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .with_context(|| format!("could not write {}", to.display()))?;
        total += n as u64;
    }
    file.flush()?;
    Ok((total, hex(&hasher.finalize())))
}

/// The downloaded archive is the one that was published, or it is deleted.
fn check(part: &Path, (size, sha256): (u64, String), config: &ModelConfig) -> Result<()> {
    let problem = if size != config.size {
        Some(format!(
            "{size} bytes arrived, {} were expected",
            config.size
        ))
    } else if !sha256.eq_ignore_ascii_case(&config.sha256) {
        Some(format!(
            "its SHA-256 is {sha256}, not the published {}",
            config.sha256
        ))
    } else {
        None
    };
    if let Some(problem) = problem {
        let _ = fs::remove_file(part);
        bail!("the downloaded speech model is not the published one: {problem}");
    }
    Ok(())
}

/// Unpack `archive` (a `.tar.bz2` whose top directory is `name`) into
/// `cache/name`, via a temporary directory renamed into place.
fn unpack(archive: &Path, cache: &Path, name: &str) -> Result<()> {
    let staging = cache.join(format!("{name}.unpacking"));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;
    let file = File::open(archive)?;
    let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(BufReader::new(file)));
    tar.unpack(&staging)
        .context("could not unpack the speech model archive")?;
    let unpacked = staging.join(name);
    if !unpacked.is_dir() {
        let _ = fs::remove_dir_all(&staging);
        bail!("the speech model archive does not contain a {name} directory");
    }
    let target = cache.join(name);
    let _ = fs::remove_dir_all(&target);
    fs::rename(&unpacked, &target).context("could not move the unpacked model into place")?;
    let _ = fs::remove_dir_all(&staging);
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_in(dir: &Path, size: u64, sha256: &str) -> ModelConfig {
        ModelConfig {
            cache_dir: dir.to_string_lossy().into_owned(),
            name: "m".into(),
            url: "http://unused.invalid/m.tar.bz2".into(),
            sha256: sha256.into(),
            size,
        }
    }

    #[test]
    fn a_download_is_hashed_as_it_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("x.part");
        let (n, sha) = copy_hashed(&mut &b"abc"[..], &out).unwrap();
        assert_eq!(n, 3);
        assert_eq!(
            sha,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(fs::read(&out).unwrap(), b"abc");
    }

    #[test]
    fn a_download_that_is_not_the_published_one_is_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("x.part");
        fs::write(&part, b"abc").unwrap();
        let wrong = config_in(tmp.path(), 3, &"0".repeat(64));
        let err = check(&part, (3, "ab".repeat(32)), &wrong).unwrap_err();
        assert!(err.to_string().contains("SHA-256"), "{err}");
        assert!(!part.exists(), "a bad download must not be kept");

        fs::write(&part, b"abc").unwrap();
        let short = config_in(tmp.path(), 10, &"0".repeat(64));
        let err = check(&part, (3, "0".repeat(64)), &short).unwrap_err();
        assert!(err.to_string().contains("10 were expected"), "{err}");
    }

    #[test]
    fn an_archive_is_unpacked_into_its_own_directory() {
        let tmp = tempfile::tempdir().unwrap();
        // Build a small .tar.bz2 with a top-level `m/` directory.
        let archive = tmp.path().join("m.tar.bz2");
        {
            let bz = bzip2::write::BzEncoder::new(
                File::create(&archive).unwrap(),
                bzip2::Compression::fast(),
            );
            let mut tar = tar::Builder::new(bz);
            for name in ModelConfig::FILES {
                let data = name.as_bytes();
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, format!("m/{name}"), data)
                    .unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
        }
        unpack(&archive, tmp.path(), "m").unwrap();
        let config = config_in(tmp.path(), 0, "");
        assert!(config.is_cached());
        assert!(!tmp.path().join("m.unpacking").exists());
    }
}
