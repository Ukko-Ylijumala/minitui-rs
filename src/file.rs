// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
A file read for a viewer: [`FileRead`] reads at most a cap of a file the
app has opened, tells a text from a binary file by a NUL among its first
bytes, and names the binary one by its magic. [`FileText`] is what was
read, as UTF-8 where it is.

The open stays the app's. Which names it may be asked for, and how one is
reached without following a link the file's owner put there, depend on
where its paths come from: below a directory a report gives, in an
encoding of its own, past `PATH_MAX`. What is checked here is asked of
the descriptor, whatever opened it: [`FileRead::read_file`] reads a
regular file and nothing else, so a FIFO swapped in for a file never
holds a key, and a device is never read. [`NotRegular`] names what was
there instead.

From the file viewers of wp-scanner and the malware scanner, which each
had the cap, the probe and the read.
*/

use std::borrow::Cow;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs::{File, FileType, Metadata};
use std::io::{self, ErrorKind, Read};
use std::os::unix::fs::FileTypeExt;
use std::str::from_utf8;

use strum::IntoStaticStr;

/* ===== consts and statics ================================================= */

static BINARY: &str = "binary data";
/// What a binary file's first bytes say it is, the longest magic first
/// where one begins another.
static MAGIC: &[(&[u8], &str)] = &[
    (b"PK\x03\x04", "ZIP archive"),
    (b"PK\x05\x06", "ZIP archive (empty)"),
    (b"\x1f\x8b", "gzip data"),
    (b"\x89PNG\r\n\x1a\n", "PNG image"),
    (b"\xff\xd8\xff", "JPEG image"),
    (b"GIF87a", "GIF image"),
    (b"GIF89a", "GIF image"),
    (b"%PDF-", "PDF document"),
    (b"\x7fELF", "ELF executable"),
    (b"BZh", "bzip2 data"),
    (b"\xfd7zXZ\x00", "xz data"),
    (b"\x28\xb5\x2f\xfd", "zstd data"),
    (b"7z\xbc\xaf\x27\x1c", "7-zip archive"),
    (b"Rar!\x1a\x07", "RAR archive"),
    (b"SQLite format 3\x00", "SQLite database"),
    (b"\x00\x00\x01\x00", "icon"),
];

/* ===== types ============================================================== */

/**
How much of a file a viewer reads, and how much of that tells a text from
a binary file.

```no_run
# use std::fs::File;
use minitui::{FileContent, FileRead, Lang, Token, Visible};

# fn demo(opened: File, name: &str) -> std::io::Result<()> {
// `opened` by the app, in whatever way its paths are safe to open
let (meta, content) = FileRead::new().read_file(&opened)?;
match content {
    FileContent::Text(text) => {
        let shown = text.lossy();
        let tokens = Lang::of(name).tokens(shown.as_ref());
        let lines = Visible::new().lines(&shown, Token::styles(&tokens));
        // ... a TextView of `lines`; `text.cut` when there was more than the cap
    }
    FileContent::Binary { kind } => println!("{kind}, {} bytes: not shown", meta.len()),
}
# Ok(())
# }
```
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileRead {
    cap: u64,
    probe: usize,
}

/// What of a file was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    Text(FileText),
    /// A NUL among its first bytes: named by its magic, not shown.
    Binary { kind: &'static str },
}

/// The bytes of a text file, up to the cap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileText {
    pub raw: Vec<u8>,
    /// The file goes on past the cap: `raw` is its beginning.
    pub cut: bool,
}

/**
What is there in place of a regular file. Its name ([`name`](Self::name))
is `a FIFO`; shown, it is `a FIFO, not a regular file`.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, IntoStaticStr)]
pub enum NotRegular {
    #[strum(serialize = "a symlink")]
    Symlink,
    #[strum(serialize = "a directory")]
    Directory,
    #[strum(serialize = "a FIFO")]
    Fifo,
    #[strum(serialize = "a socket")]
    Socket,
    /// A character or a block device.
    #[strum(serialize = "a device")]
    Device,
    #[strum(serialize = "a file of no known kind")]
    Unknown,
}

/* ===== FileRead =========================================================== */

impl Default for FileRead {
    fn default() -> Self {
        Self { cap: Self::CAP, probe: Self::PROBE }
    }
}

impl FileRead {
    /// How much is read unless set: plenty for any script worth reading,
    /// and still a text a terminal can be scrolled through.
    pub const CAP: u64 = 1024 * 1024;
    /// How much is looked at for a NUL unless set.
    pub const PROBE: usize = 8 * 1024;

    pub fn new() -> Self {
        Self::default()
    }

    /// Read at most `bytes` of a file.
    pub fn cap(mut self, bytes: u64) -> Self {
        self.cap = bytes;
        self
    }

    /// A NUL among the first `bytes` makes a file binary; with 0, nothing does.
    pub fn probe(mut self, bytes: usize) -> Self {
        self.probe = bytes;
        self
    }

    /// The cap in bytes, for the line that says how much of a file is shown.
    pub fn limit(&self) -> u64 {
        self.cap
    }

    /**
    Read `file` if it is a regular file: asked of the descriptor, so of
    what was opened and not of a name that may be something else by now.
    Anything else is refused unread, as an `InvalidInput` error that holds
    a [`NotRegular`] ([`NotRegular::in_error`]).

    The metadata is that of the opened file too, and the one to show: a
    stat of the name from before the open is of another moment.
    */
    pub fn read_file(&self, file: &File) -> io::Result<(Metadata, FileContent)> {
        let meta = file.metadata()?;
        if let Some(kind) = NotRegular::of(meta.file_type()) {
            return Err(io::Error::new(ErrorKind::InvalidInput, kind));
        }
        Ok((meta, self.read(file)?))
    }

    /**
    Read `from` up to the cap. Whether there was more is asked of the read
    itself, by one byte past the cap, and not of a size taken earlier: a
    file that grew since is cut and says so.
    */
    pub fn read(&self, from: impl Read) -> io::Result<FileContent> {
        let mut raw: Vec<u8> = Vec::new();
        from.take(self.cap.saturating_add(1)).read_to_end(&mut raw)?;
        let cap = usize::try_from(self.cap).unwrap_or(usize::MAX);
        let cut = raw.len() > cap;
        raw.truncate(cap);
        Ok(match raw[..raw.len().min(self.probe)].contains(&0) {
            true => FileContent::Binary { kind: magic_kind(&raw) },
            false => FileContent::Text(FileText { raw, cut }),
        })
    }
}

/* ===== FileText =========================================================== */

impl FileText {
    /**
    The text when all of it is UTF-8. A character the cap fell inside is
    left out: the text ends before it, and is UTF-8 still.
    */
    pub fn utf8(&self) -> Option<&str> {
        match from_utf8(&self.raw) {
            Ok(text) => Some(text),
            Err(e) if self.cut && e.error_len().is_none() => from_utf8(&self.raw[..e.valid_up_to()]).ok(),
            Err(_) => None,
        }
    }

    /**
    The text as UTF-8 with `U+FFFD` for what is not. A character the cap
    fell inside is left out, as by [`utf8`](Self::utf8): the cut is said
    by `cut`, not by a replacement character the file does not have.
    */
    pub fn lossy(&self) -> Cow<'_, str> {
        let cut_short = self.raw.utf8_chunks().last().map_or(0, |last| {
            let tail = last.invalid();
            let unfinished = self.cut && from_utf8(tail).is_err_and(|e| e.error_len().is_none());
            if unfinished { tail.len() } else { 0 }
        });
        String::from_utf8_lossy(&self.raw[..self.raw.len() - cut_short])
    }

    /// The text with every byte a character, as latin-1 has them: what an
    /// old upload is written in, and never wrong about where a byte is.
    pub fn latin1(&self) -> String {
        self.raw.iter().map(|b| char::from(*b)).collect()
    }
}

/* ===== NotRegular ========================================================= */

impl NotRegular {
    /// What `kind` is, when it is no regular file.
    pub fn of(kind: FileType) -> Option<Self> {
        if kind.is_file() {
            None
        } else if kind.is_symlink() {
            Some(Self::Symlink)
        } else if kind.is_dir() {
            Some(Self::Directory)
        } else if kind.is_fifo() {
            Some(Self::Fifo)
        } else if kind.is_socket() {
            Some(Self::Socket)
        } else if kind.is_char_device() || kind.is_block_device() {
            Some(Self::Device)
        } else {
            Some(Self::Unknown)
        }
    }

    /// With its article: `a FIFO`.
    pub fn name(self) -> &'static str {
        self.into()
    }

    /// The one [`FileRead::read_file`] refused with, if `e` is such a refusal.
    pub fn in_error(e: &io::Error) -> Option<Self> {
        e.get_ref()?.downcast_ref::<Self>().copied()
    }
}

impl Display for NotRegular {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}, not a regular file", self.name())
    }
}

impl Error for NotRegular {}

/* ===== free functions ===================================================== */

/// What a binary file's first bytes say it is.
fn magic_kind(head: &[u8]) -> &'static str {
    MAGIC.iter().find(|(magic, _)| head.starts_with(magic)).map_or(BINARY, |(_, kind)| kind)
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use std::env::temp_dir;
    use std::fs::{create_dir_all, remove_dir_all, symlink_metadata, write};
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::process;

    use super::*;

    /// A directory of this test's own.
    fn scratch(name: &str) -> PathBuf {
        let dir = temp_dir().join(format!("minitui-file-{name}-{}", process::id()));
        let _ = remove_dir_all(&dir);
        create_dir_all(&dir).unwrap();
        dir
    }

    fn text(content: FileContent) -> FileText {
        match content {
            FileContent::Text(text) => text,
            FileContent::Binary { kind } => panic!("binary: {kind}"),
        }
    }

    fn cut_at(raw: &[u8], cap: u64) -> FileText {
        text(FileRead::new().cap(cap).read(raw).unwrap())
    }

    #[test]
    fn at_most_the_cap() {
        let cap = usize::try_from(FileRead::CAP).unwrap();
        let read = text(FileRead::new().read(b"x".repeat(cap + 10).as_slice()).unwrap());
        assert_eq!((read.raw.len(), read.cut), (cap, true));
        // a file that ends at the cap is all there
        let read = text(FileRead::new().read(b"x".repeat(cap).as_slice()).unwrap());
        assert_eq!((read.raw.len(), read.cut), (cap, false));
        assert_eq!(cut_at(b"<?php echo 1;", 5), FileText { raw: b"<?php".to_vec(), cut: true });
        assert_eq!(cut_at(b"", 5), FileText::default());
        assert_eq!(cut_at(b"abc", 0), FileText { raw: Vec::new(), cut: true });
        assert_eq!(cut_at(b"abc", u64::MAX), FileText { raw: b"abc".to_vec(), cut: false });
        assert_eq!(FileRead::new().cap(7).limit(), 7);
    }

    #[test]
    fn a_nul_among_the_first_bytes_is_binary_and_named() {
        for (body, kind) in [
            (b"PK\x03\x04\x14\x00\x00\x00".as_slice(), "ZIP archive"),
            (b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".as_slice(), "PNG image"),
            (b"\x7fELF\x02\x01\x01\x00".as_slice(), "ELF executable"),
            (b"<?php \x00\x01".as_slice(), "binary data"),
        ] {
            assert_eq!(FileRead::new().read(body).unwrap(), FileContent::Binary { kind }, "{kind}");
        }
        // a magic without a NUL is a text that begins oddly
        assert!(matches!(FileRead::new().read(b"%PDF-1.7\n".as_slice()).unwrap(), FileContent::Text(_)));
        // a NUL past the probe does not make a text binary
        let mut late = b"a".repeat(FileRead::PROBE);
        late.push(0);
        assert!(matches!(FileRead::new().read(late.as_slice()).unwrap(), FileContent::Text(_)));
        assert!(matches!(FileRead::new().probe(late.len()).read(late.as_slice()).unwrap(), FileContent::Binary { .. }));
        assert!(matches!(FileRead::new().probe(0).read(b"\0".as_slice()).unwrap(), FileContent::Text(_)));
    }

    #[test]
    fn utf8_where_it_is_and_cut_inside_a_character_still() {
        let whole = FileText { raw: "<?php // sähkö".as_bytes().to_vec(), cut: false };
        assert_eq!(whole.utf8(), Some("<?php // sähkö"));
        assert_eq!(whole.lossy(), "<?php // sähkö");
        // the cap between the two bytes of the last ö
        let raw = "sähkö".as_bytes();
        let cut = cut_at(raw, raw.len() as u64 - 1);
        assert_eq!((cut.utf8(), cut.lossy().as_ref()), (Some("sähk"), "sähk"));
        // the same bytes as all there is: no UTF-8, and the byte shown as what it is not
        let short = FileText { raw: cut.raw.clone(), cut: false };
        assert_eq!((short.utf8(), short.lossy().as_ref()), (None, "sähk\u{fffd}"));
        // latin-1: not UTF-8, cut or not; a byte that is wrong in the middle stays
        let latin = FileText { raw: b"<?php // caf\xe9 au lait".to_vec(), cut: true };
        assert_eq!(latin.utf8(), None);
        assert_eq!(latin.lossy(), "<?php // caf\u{fffd} au lait");
        assert_eq!(latin.latin1(), "<?php // café au lait");
        // an invalid byte at the cut is no unfinished character
        let bad = FileText { raw: b"a\xff".to_vec(), cut: true };
        assert_eq!((bad.utf8(), bad.lossy().as_ref()), (None, "a\u{fffd}"));
        assert_eq!(FileText::default().lossy(), "");
    }

    #[test]
    fn a_regular_file_only_asked_of_what_was_opened() {
        let dir = scratch("regular");
        write(dir.join("x.php"), b"<?php echo 1;\n").unwrap();
        let (meta, content) = FileRead::new().read_file(&File::open(dir.join("x.php")).unwrap()).unwrap();
        assert_eq!(meta.len(), 14);
        assert_eq!(text(content).raw, b"<?php echo 1;\n");

        let refused = |file: File| {
            let e = FileRead::new().read_file(&file).unwrap_err();
            (e.kind(), NotRegular::in_error(&e), e.to_string())
        };
        assert_eq!(
            refused(File::open(&dir).unwrap()),
            (ErrorKind::InvalidInput, Some(NotRegular::Directory), "a directory, not a regular file".to_string())
        );
        assert_eq!(
            refused(File::open("/dev/null").unwrap()),
            (ErrorKind::InvalidInput, Some(NotRegular::Device), "a device, not a regular file".to_string())
        );
        assert_eq!(NotRegular::in_error(&io::Error::other("something else")), None);
        remove_dir_all(dir).unwrap();
    }

    #[test]
    fn what_a_name_is_when_it_is_no_file() {
        let dir = scratch("kinds");
        write(dir.join("real.php"), b"<?php").unwrap();
        symlink(dir.join("real.php"), dir.join("link.php")).unwrap();
        let _socket = UnixListener::bind(dir.join("socket")).unwrap();
        let kind = |name: &str| NotRegular::of(symlink_metadata(dir.join(name)).unwrap().file_type());
        assert_eq!(kind("real.php"), None);
        assert_eq!(kind("link.php"), Some(NotRegular::Symlink));
        assert_eq!(kind("socket"), Some(NotRegular::Socket));
        assert_eq!(kind("."), Some(NotRegular::Directory));
        assert_eq!(NotRegular::Fifo.name(), "a FIFO");
        assert_eq!(NotRegular::Fifo.to_string(), "a FIFO, not a regular file");
        remove_dir_all(dir).unwrap();
    }
}
