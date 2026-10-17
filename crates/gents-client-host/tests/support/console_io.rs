use std::io::{self, Read, Write};

pub fn drain(
    reader: &mut impl Read,
    output: &mut impl Write,
    mut report_cursor: impl FnMut() -> io::Result<()>,
) -> io::Result<u64> {
    const QUERY: &[u8] = b"\x1b[6n";
    let mut matched = 0;
    let mut total = 0;
    let mut buffer = [0; 8192];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        output.write_all(&buffer[..read])?;
        total += read as u64;
        for &byte in &buffer[..read] {
            if byte == QUERY[matched] {
                matched += 1;
                if matched == QUERY.len() {
                    report_cursor()?;
                    matched = 0;
                }
            } else {
                matched = usize::from(byte == QUERY[0]);
            }
        }
    }
}

#[test]
fn cursor_queries_are_answered_across_read_boundaries_without_losing_output() {
    let bytes = b"console\x1b[6nready\x1b[\x1b[6n";
    for split in 0..=bytes.len() {
        let mut reader = io::Cursor::new(&bytes[..split]).chain(io::Cursor::new(&bytes[split..]));
        let mut output = Vec::new();
        let mut reports = 0;
        let copied = drain(&mut reader, &mut output, || {
            reports += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(copied, bytes.len() as u64);
        assert_eq!(output, bytes);
        assert_eq!(reports, 2);
    }
}

#[test]
fn cursor_reply_failures_are_reported() {
    let error = drain(&mut &b"\x1b[6n"[..], &mut Vec::new(), || {
        Err(io::ErrorKind::BrokenPipe.into())
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
}
