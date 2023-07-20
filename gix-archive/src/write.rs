use crate::{Error, Format, Options};
use gix_worktree_stream::Stream;

/// Write the worktree `stream` to `out` configured according to `opts`.
///
/// ### Performance
///
/// * The caller should be sure `out` is fast enough. If in doubt, wrap in [`std::io::BufWriter`].
/// * Further, big files aren't suitable for archival into `tar` archives as they require the size of the stream to be known
///   prior to writing the header of each entry.
pub fn write_stream(stream: &mut Stream, out: impl std::io::Write, opts: Options) -> Result<(), Error> {
    let mut state = State::new(opts.format, out);
    #[cfg_attr(not(any(feature = "tar")), allow(irrefutable_let_patterns))]
    if let State::Internal(out) = &mut state {
        let read = stream.as_read_mut();
        std::io::copy(read, out)?;
        return Ok(());
    }

    #[cfg(feature = "tar")]
    {
        let mtime_seconds_since_epoch = opts
            .modification_time
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());

        while let Some(mut entry) = stream.next_entry()? {
            match &mut state {
                State::Internal(_) => unreachable!("handle outside of entry loop"),
                #[cfg(feature = "tar")]
                State::Tar((ar, buf)) => {
                    let mut header = tar::Header::new_gnu();
                    if let Some(mtime) = mtime_seconds_since_epoch {
                        header.set_mtime(mtime);
                    }
                    header.set_entry_type(tar_entry_type(entry.mode));
                    header.set_mode(if matches!(entry.mode, gix_object::tree::EntryMode::BlobExecutable) {
                        0o755
                    } else {
                        0o644
                    });
                    buf.clear();
                    std::io::copy(&mut entry, buf)?;

                    let path = gix_path::from_bstr(add_prefix(entry.relative_path(), opts.tree_prefix.as_ref()));
                    header.set_size(buf.len() as u64);

                    if entry.mode == gix_object::tree::EntryMode::Link {
                        use bstr::ByteSlice;
                        let target = gix_path::from_bstr(buf.as_bstr());
                        header.set_entry_type(tar::EntryType::Symlink);
                        header.set_size(0);
                        ar.append_link(&mut header, path, target)?;
                    } else {
                        ar.append_data(&mut header, path, buf.as_slice())?;
                    }
                }
            }
        }

        match state {
            State::Internal(_) => {}
            #[cfg(feature = "tar")]
            State::Tar((mut ar, _)) => {
                ar.finish()?;
            }
        }
    }
    Ok(())
}

#[cfg(feature = "tar")]
fn tar_entry_type(mode: gix_object::tree::EntryMode) -> tar::EntryType {
    use gix_object::tree::EntryMode;
    use tar::EntryType;
    match mode {
        EntryMode::Tree | EntryMode::Commit => EntryType::Directory,
        EntryMode::Blob => EntryType::Regular,
        EntryMode::BlobExecutable => EntryType::Regular,
        EntryMode::Link => EntryType::Link,
    }
}

#[cfg(feature = "tar")]
fn add_prefix<'a>(relative_path: &'a bstr::BStr, prefix: Option<&bstr::BString>) -> std::borrow::Cow<'a, bstr::BStr> {
    use std::borrow::Cow;
    match prefix {
        None => Cow::Borrowed(relative_path),
        Some(prefix) => {
            use bstr::ByteVec;
            let mut buf = prefix.clone();
            buf.push_str(relative_path);
            Cow::Owned(buf)
        }
    }
}

enum State<W: std::io::Write> {
    Internal(W),
    #[cfg(feature = "tar")]
    Tar((tar::Builder<W>, Vec<u8>)),
}

impl<W: std::io::Write> State<W> {
    pub fn new(format: Format, out: W) -> Self {
        match format {
            Format::InternalTransientNonPersistable => State::Internal(out),
            #[cfg(feature = "tar")]
            Format::Tar => State::Tar((
                {
                    let mut ar = tar::Builder::new(out);
                    ar.mode(tar::HeaderMode::Deterministic);
                    ar
                },
                Vec::with_capacity(64 * 1024),
            )),
        }
    }
}
