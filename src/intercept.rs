use std::ffi::OsStr;
use std::os::fd::RawFd;
use std::path::Path;

use globset::Glob;
use globset::GlobSet;
use globset::GlobSetBuilder;

use crate::provider::ContentProvider;

pub(crate) struct InterceptMatcher {
    globs: GlobSet,
}

impl InterceptMatcher {
    pub fn new(patterns: &[String]) -> anyhow::Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for pat in patterns {
            builder.add(Glob::new(pat)?);
        }
        Ok(Self {
            globs: builder.build()?,
        })
    }

    pub fn is_intercepted(&self, filename: &OsStr) -> bool {
        self.globs.is_match(Path::new(filename))
    }

    /// Read the real file via pread, then append extra content from provider.
    pub fn assemble(
        real_fd: RawFd,
        real_size: u64,
        rel_path: &Path,
        provider: &dyn ContentProvider,
    ) -> anyhow::Result<Vec<u8>> {
        let mut buf = vec![0u8; real_size as usize];
        let mut offset = 0usize;
        while offset < real_size as usize {
            let n = unsafe {
                libc::pread(
                    real_fd,
                    buf[offset..].as_mut_ptr().cast(),
                    buf.len() - offset,
                    offset as libc::off_t,
                )
            };
            if n <= 0 {
                break;
            }
            offset += n as usize;
        }
        buf.truncate(offset);

        let extra = provider.extra_content(rel_path)?;
        if !extra.is_empty() {
            buf.push(b'\n');
            buf.extend_from_slice(&extra);
        }
        Ok(buf)
    }

    /// Compute total size: real file size + 1 (newline) + extra content length.
    /// Returns None if no extra content.
    pub fn inflated_size(
        real_size: u64,
        rel_path: &Path,
        provider: &dyn ContentProvider,
    ) -> Option<u64> {
        let extra = provider.extra_content(rel_path).ok()?;
        if extra.is_empty() {
            return None;
        }
        Some(real_size + 1 + extra.len() as u64)
    }
}
