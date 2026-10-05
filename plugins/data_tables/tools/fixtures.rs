//! Deterministic builders for the test fixtures: spreadsheets, Parquet and Arrow
//! files. The `gen_fixtures` example writes them under `tests/fixtures`, and the
//! unit tests build the same bytes in memory, so a fixture is never hand-made.
#![allow(dead_code)]
use std::io::{Cursor, Write};
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Date32Array, Decimal128Array, Float64Array, Int32Array,
    Int64Array, ListArray, StringArray, StructArray, TimestampMicrosecondArray, UInt64Array,
};
use arrow::datatypes::{DataType, Field, Fields, Int32Type, Schema, TimeUnit};
use arrow::ipc::CompressionType;
use arrow::ipc::writer::{FileWriter, IpcWriteOptions, StreamWriter};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// One XLSX cell.
#[derive(Clone, Debug)]
pub enum X {
    /// Nothing.
    Empty,
    /// A shared string.
    S(&'static str),
    /// An inline string.
    I(&'static str),
    /// A number, written as given.
    N(&'static str),
    /// A boolean.
    B(bool),
    /// A date serial in a date-formatted cell.
    D(&'static str),
    /// A date and time serial in a date-time-formatted cell.
    T(&'static str),
    /// A formula string result.
    F(&'static str),
    /// An error value.
    E(&'static str),
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn column_letters(mut i: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (i % 26) as u8);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn zip_of(parts: &[(&str, Vec<u8>, bool)]) -> Vec<u8> {
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data, stored) in parts {
        let method = if *stored {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        zw.start_file(
            *name,
            SimpleFileOptions::default().compression_method(method),
        )
        .unwrap();
        zw.write_all(data).unwrap();
    }
    zw.finish().unwrap().into_inner()
}

/// An XLSX workbook of `sheets` (name, rows).
pub fn xlsx(sheets: &[(&str, Vec<Vec<X>>)]) -> Vec<u8> {
    xlsx_with(sheets, false)
}

/// An XLSX workbook, optionally on the 1904 date system.
pub fn xlsx_with(sheets: &[(&str, Vec<Vec<X>>)], date1904: bool) -> Vec<u8> {
    let mut shared: Vec<&str> = Vec::new();
    let mut sheet_xml = Vec::new();
    for (_, rows) in sheets {
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>",
        );
        for (r, row) in rows.iter().enumerate() {
            xml.push_str(&format!("<row r=\"{}\">", r + 1));
            for (c, cell) in row.iter().enumerate() {
                let at = format!("{}{}", column_letters(c), r + 1);
                match cell {
                    X::Empty => {}
                    X::S(s) => {
                        let i = shared.iter().position(|x| x == s).unwrap_or_else(|| {
                            shared.push(s);
                            shared.len() - 1
                        });
                        xml.push_str(&format!("<c r=\"{at}\" t=\"s\"><v>{i}</v></c>"));
                    }
                    X::I(s) => xml.push_str(&format!(
                        "<c r=\"{at}\" t=\"inlineStr\"><is><t>{}</t></is></c>",
                        esc(s)
                    )),
                    X::N(n) => xml.push_str(&format!("<c r=\"{at}\"><v>{n}</v></c>")),
                    X::B(b) => xml.push_str(&format!(
                        "<c r=\"{at}\" t=\"b\"><v>{}</v></c>",
                        u8::from(*b)
                    )),
                    X::D(n) => xml.push_str(&format!("<c r=\"{at}\" s=\"1\"><v>{n}</v></c>")),
                    X::T(n) => xml.push_str(&format!("<c r=\"{at}\" s=\"2\"><v>{n}</v></c>")),
                    X::F(s) => xml.push_str(&format!(
                        "<c r=\"{at}\" t=\"str\"><f>A1</f><v>{}</v></c>",
                        esc(s)
                    )),
                    X::E(s) => {
                        xml.push_str(&format!("<c r=\"{at}\" t=\"e\"><v>{}</v></c>", esc(s)))
                    }
                }
            }
            xml.push_str("</row>");
        }
        xml.push_str("</sheetData></worksheet>");
        sheet_xml.push(xml);
    }
    let mut parts: Vec<(String, Vec<u8>, bool)> = Vec::new();
    parts.push(("[Content_Types].xml".into(), b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/></Types>".to_vec(), false));
    parts.push(("_rels/.rels".into(), b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>".to_vec(), false));
    let mut wb = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><workbookPr{}/><sheets>",
        if date1904 { " date1904=\"1\"" } else { "" }
    );
    let mut rels = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for (i, (name, _)) in sheets.iter().enumerate() {
        wb.push_str(&format!(
            "<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rId{}\"/>",
            esc(name),
            i + 1,
            i + 1
        ));
        rels.push_str(&format!("<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{}.xml\"/>", i + 1, i + 1));
    }
    wb.push_str("</sheets></workbook>");
    rels.push_str("</Relationships>");
    parts.push(("xl/workbook.xml".into(), wb.into_bytes(), false));
    parts.push((
        "xl/_rels/workbook.xml.rels".into(),
        rels.into_bytes(),
        false,
    ));
    parts.push(("xl/styles.xml".into(), b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><numFmts count=\"1\"><numFmt numFmtId=\"164\" formatCode=\"yyyy\\-mm\\-dd\\ hh:mm\"/></numFmts><cellXfs count=\"3\"><xf numFmtId=\"0\"/><xf numFmtId=\"14\"/><xf numFmtId=\"164\"/></cellXfs></styleSheet>".to_vec(), false));
    let mut sst = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"{0}\" uniqueCount=\"{0}\">",
        shared.len()
    );
    for s in &shared {
        sst.push_str(&format!(
            "<si><t xml:space=\"preserve\">{}</t></si>",
            esc(s)
        ));
    }
    sst.push_str("</sst>");
    parts.push(("xl/sharedStrings.xml".into(), sst.into_bytes(), false));
    for (i, xml) in sheet_xml.into_iter().enumerate() {
        parts.push((
            format!("xl/worksheets/sheet{}.xml", i + 1),
            xml.into_bytes(),
            false,
        ));
    }
    let refs: Vec<(&str, Vec<u8>, bool)> = parts
        .iter()
        .map(|(n, d, s)| (n.as_str(), d.clone(), *s))
        .collect();
    zip_of(&refs)
}

/// One ODS cell.
#[derive(Clone, Debug)]
pub enum O {
    /// Nothing, repeated this many columns.
    Gap(u32),
    /// A string.
    S(&'static str),
    /// A float.
    F(&'static str),
    /// A boolean.
    B(bool),
    /// A date or date-time value.
    D(&'static str),
    /// A percentage.
    P(&'static str),
}

/// An ODS workbook of `sheets` (name, rows); a row is a (repeat count, cells) pair.
pub fn ods(sheets: &[(&str, Vec<(u32, Vec<O>)>)]) -> Vec<u8> {
    let mut body = String::new();
    for (name, rows) in sheets {
        body.push_str(&format!("<table:table table:name=\"{}\">", esc(name)));
        for (repeat, cells) in rows {
            body.push_str(&format!(
                "<table:table-row table:number-rows-repeated=\"{repeat}\">"
            ));
            for c in cells {
                match c {
                    O::Gap(n) => body.push_str(&format!("<table:table-cell table:number-columns-repeated=\"{n}\"/>")),
                    O::S(s) => {
                        let paragraphs: Vec<String> = s.split('\n').map(|p| format!("<text:p>{}</text:p>", esc(p).replace("  ", " <text:s/>"))).collect();
                        body.push_str(&format!("<table:table-cell office:value-type=\"string\">{}</table:table-cell>", paragraphs.concat()));
                    }
                    O::F(v) => body.push_str(&format!("<table:table-cell office:value-type=\"float\" office:value=\"{v}\"><text:p>{v}</text:p></table:table-cell>")),
                    O::P(v) => body.push_str(&format!("<table:table-cell office:value-type=\"percentage\" office:value=\"{v}\"><text:p>x</text:p></table:table-cell>")),
                    O::B(b) => body.push_str(&format!("<table:table-cell office:value-type=\"boolean\" office:boolean-value=\"{b}\"><text:p>{b}</text:p></table:table-cell>")),
                    O::D(v) => body.push_str(&format!("<table:table-cell office:value-type=\"date\" office:date-value=\"{v}\"><text:p>{v}</text:p></table:table-cell>")),
                }
            }
            body.push_str("</table:table-row>");
        }
        body.push_str("</table:table>");
    }
    let content = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><office:document-content xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\" xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\"><office:body><office:spreadsheet>{body}</office:spreadsheet></office:body></office:document-content>"
    );
    zip_of(&[
        ("mimetype", b"application/vnd.oasis.opendocument.spreadsheet".to_vec(), true),
        ("content.xml", content.into_bytes(), false),
        ("META-INF/manifest.xml", b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><manifest:manifest xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\"/>".to_vec(), false),
    ])
}

/// A workbook whose sheet inflates far past its stored size: a decompression bomb.
pub fn zip_bomb() -> Vec<u8> {
    let mut sheet = b"<?xml version=\"1.0\"?><worksheet><sheetData>".to_vec();
    let filler = vec![b' '; 1 << 20];
    for _ in 0..300 {
        sheet.extend_from_slice(&filler);
    }
    sheet.extend_from_slice(b"</sheetData></worksheet>");
    let ok = xlsx(&[("S", vec![vec![X::S("a")]])]);
    // Rebuild the workbook with the oversized sheet in place of the real one.
    let mut src = zip::ZipArchive::new(Cursor::new(ok)).unwrap();
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..src.len() {
        let mut f = src.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut data).unwrap();
        if name == "xl/worksheets/sheet1.xml" {
            data = sheet.clone();
        }
        zw.start_file(
            name,
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .unwrap();
        zw.write_all(&data).unwrap();
    }
    zw.finish().unwrap().into_inner()
}

/// The reference table the Parquet and Arrow fixtures hold.
pub fn sample_batch() -> RecordBatch {
    let nested = StructArray::from(vec![
        (
            Arc::new(Field::new("x", DataType::Int32, true)),
            Arc::new(Int32Array::from(vec![Some(1), None, Some(3)])) as ArrayRef,
        ),
        (
            Arc::new(Field::new("y", DataType::Utf8, true)),
            Arc::new(StringArray::from(vec![Some("a"), Some("b"), None])) as ArrayRef,
        ),
    ]);
    let tags = ListArray::from_iter_primitive::<Int32Type, _, _>(vec![
        Some(vec![Some(1), Some(2)]),
        Some(vec![]),
        None,
    ]);
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("score", DataType::Float64, true),
        Field::new("active", DataType::Boolean, true),
        Field::new("born", DataType::Date32, true),
        Field::new(
            "seen",
            DataType::Timestamp(TimeUnit::Microsecond, None),
            true,
        ),
        Field::new("price", DataType::Decimal128(10, 2), true),
        Field::new("big", DataType::UInt64, true),
        Field::new("nested", nested.data_type().clone(), true),
        Field::new("tags", tags.data_type().clone(), true),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![1, 2, 3])),
            Arc::new(StringArray::from(vec![Some("Ana"), Some(""), None])),
            Arc::new(Float64Array::from(vec![Some(1.5), None, Some(f64::NAN)])),
            Arc::new(BooleanArray::from(vec![Some(true), Some(false), None])),
            Arc::new(Date32Array::from(vec![Some(19782), None, Some(0)])),
            Arc::new(TimestampMicrosecondArray::from(vec![
                Some(1_709_209_845_500_000),
                None,
                Some(0),
            ])),
            Arc::new(
                Decimal128Array::from(vec![Some(12345), None, Some(-5)])
                    .with_precision_and_scale(10, 2)
                    .unwrap(),
            ),
            Arc::new(UInt64Array::from(vec![Some(u64::MAX), Some(1), None])),
            Arc::new(nested),
            Arc::new(tags),
        ],
    )
    .unwrap()
}

/// A Parquet file of `batch` written with `codec`.
pub fn parquet(batch: &RecordBatch, codec: Compression) -> Vec<u8> {
    let props = WriterProperties::builder()
        .set_compression(codec)
        .set_created_by("gents data_tables fixtures".into())
        .build();
    let mut out = Vec::new();
    let mut w = ArrowWriter::try_new(&mut out, batch.schema(), Some(props)).unwrap();
    w.write(batch).unwrap();
    w.close().unwrap();
    out
}

/// An Arrow IPC file of `batch`, optionally compressed.
pub fn arrow_file(batch: &RecordBatch, codec: Option<CompressionType>) -> Vec<u8> {
    let options = IpcWriteOptions::default()
        .try_with_compression(codec)
        .unwrap();
    let mut w = FileWriter::try_new_with_options(Vec::new(), &batch.schema(), options).unwrap();
    w.write(batch).unwrap();
    w.into_inner().unwrap()
}

/// An Arrow IPC stream of `batch`.
pub fn arrow_stream(batch: &RecordBatch) -> Vec<u8> {
    let mut w = StreamWriter::try_new(Vec::new(), &batch.schema()).unwrap();
    w.write(batch).unwrap();
    w.into_inner().unwrap()
}

/// A flat batch of two integer columns, `rows` long, for size tests.
pub fn ints(rows: i64) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("a", DataType::Int64, false),
        Field::new("b", DataType::Int64, false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(0..rows)),
            Arc::new(Int64Array::from_iter_values((0..rows).map(|i| i % 7))),
        ],
    )
    .unwrap()
}

#[allow(unused)]
fn unused(_: Fields) {}
