// Word, Excel and PowerPoint files are zip archives of XML, which neither the
// API nor Claude Code can read as they are. Their text is pulled out here so a
// dropped .docx, .xlsx or .pptx can be asked about like any text file.
//
// Only the text: no layout, no images, no charts. A spreadsheet comes out as
// tab-separated rows, sheet by sheet, with formulas as their last saved value.

use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use zip::ZipArchive;

/// One XML part is never read past this, however much the archive claims.
const MAX_PART: u64 = 64 * 1024 * 1024;
/// The extracted text stops here; a longer document is cut and says so.
const MAX_TEXT: usize = 2_000_000;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Word,
    Excel,
    PowerPoint,
}

fn kind_of(path: &Path) -> Option<Result<Kind, String>> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    match ext.as_str() {
        "docx" | "docm" => Some(Ok(Kind::Word)),
        "xlsx" | "xlsm" => Some(Ok(Kind::Excel)),
        "pptx" | "pptm" => Some(Ok(Kind::PowerPoint)),
        "doc" | "xls" | "ppt" => Some(Err(format!(
            "the old .{ext} format is not supported — save it as .{ext}x"
        ))),
        _ => None,
    }
}

/// The text of an Office document. `None` when the file is not one.
pub fn extract_text(path: &Path) -> Option<Result<String, String>> {
    let kind = match kind_of(path)? {
        Ok(kind) => kind,
        Err(why) => return Some(Err(why)),
    };
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) => return Some(Err(e.to_string())),
    };
    Some(extract(file, kind))
}

/// Writes the document's text next to it as `<name>.txt` and returns that path,
/// for Claude Code to read. `None` when the file is not an Office document.
pub fn text_copy(path: &Path) -> Option<Result<PathBuf, String>> {
    let text = match extract_text(path)? {
        Ok(text) => text,
        Err(why) => return Some(Err(why)),
    };
    let mut name = path.file_name()?.to_os_string();
    name.push(".txt");
    let dest = path.with_file_name(name);
    Some(std::fs::write(&dest, text).map(|_| dest).map_err(|e| e.to_string()))
}

fn extract<R: Read + Seek>(source: R, kind: Kind) -> Result<String, String> {
    let mut zip = ZipArchive::new(source).map_err(|_| "the file is not a valid Office document".to_string())?;
    let mut text = match kind {
        Kind::Word => word(&mut zip)?,
        Kind::Excel => excel(&mut zip)?,
        Kind::PowerPoint => powerpoint(&mut zip)?,
    };
    if text.trim().is_empty() {
        return Err("the document has no text".into());
    }
    if text.len() > MAX_TEXT {
        let mut cut = MAX_TEXT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n\n[The document is longer; the rest was left out.]");
    }
    Ok(text)
}

fn part<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str) -> Option<String> {
    let entry = zip.by_name(name).ok()?;
    let mut xml = String::new();
    entry.take(MAX_PART).read_to_string(&mut xml).ok()?;
    Some(xml)
}

/// `prefix1.xml`, `prefix2.xml`, … in numeric order — slides and sheets.
fn numbered_parts<R: Read + Seek>(zip: &ZipArchive<R>, prefix: &str) -> Vec<String> {
    let mut found: Vec<(u32, String)> = zip
        .file_names()
        .filter_map(|name| {
            let n = name.strip_prefix(prefix)?.strip_suffix(".xml")?.parse().ok()?;
            Some((n, name.to_string()))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, name)| name).collect()
}

// ── Word ──────────────────────────────────────────────────────────────────────

fn word<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<String, String> {
    let xml = part(zip, "word/document.xml").ok_or("the document has no body")?;
    Ok(runs(&xml))
}

/// Text runs and paragraph breaks, the same in WordprocessingML (`w:t`, `w:p`)
/// and DrawingML (`a:t`, `a:p`).
fn runs(xml: &str) -> String {
    let mut out = String::new();
    let mut in_text = false;
    for node in Scanner::new(xml) {
        match node {
            Node::Open { name, empty, .. } => match local(name) {
                "t" => in_text = !empty,
                "tab" => out.push('\t'),
                "br" | "cr" => out.push('\n'),
                _ => {}
            },
            Node::Close(name) => match local(name) {
                "t" => in_text = false,
                "p" => out.push('\n'),
                _ => {}
            },
            Node::Text(text) if in_text => out.push_str(&unescape(text)),
            Node::Text(_) => {}
        }
    }
    out
}

// ── PowerPoint ────────────────────────────────────────────────────────────────

fn powerpoint<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<String, String> {
    let mut out = String::new();
    for (i, name) in numbered_parts(zip, "ppt/slides/slide").iter().enumerate() {
        let Some(xml) = part(zip, name) else { continue };
        out.push_str(&format!("--- Slide {} ---\n{}\n", i + 1, runs(&xml).trim_end()));
    }
    Ok(out)
}

// ── Excel ─────────────────────────────────────────────────────────────────────

fn excel<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<String, String> {
    let strings = part(zip, "xl/sharedStrings.xml").map(|xml| shared_strings(&xml)).unwrap_or_default();
    let dates = part(zip, "xl/styles.xml").map(|xml| date_styles(&xml)).unwrap_or_default();
    let names = part(zip, "xl/workbook.xml").map(|xml| sheet_names(&xml)).unwrap_or_default();

    let sheets = numbered_parts(zip, "xl/worksheets/sheet");
    let mut out = String::new();
    for (i, file) in sheets.iter().enumerate() {
        let Some(xml) = part(zip, file) else { continue };
        let rows = sheet(&xml, &strings, &dates);
        if rows.is_empty() {
            continue;
        }
        // Sheet files are usually in the workbook's order; when the counts do
        // not match, a number is more honest than a guessed name.
        let title = match names.get(i).filter(|_| names.len() == sheets.len()) {
            Some(name) => name.clone(),
            None => format!("Sheet {}", i + 1),
        };
        out.push_str(&format!("--- {title} ---\n{rows}\n"));
    }
    Ok(out)
}

fn shared_strings(xml: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut current = String::new();
    let mut in_text = false;
    let mut phonetic = false;
    for node in Scanner::new(xml) {
        match node {
            Node::Open { name, empty, .. } => match local(name) {
                "si" => current.clear(),
                // Phonetic guides repeat the text in another script.
                "rPh" => phonetic = !empty,
                "t" => in_text = !empty && !phonetic,
                _ => {}
            },
            Node::Close(name) => match local(name) {
                "si" => strings.push(std::mem::take(&mut current)),
                "rPh" => phonetic = false,
                "t" => in_text = false,
                _ => {}
            },
            Node::Text(text) if in_text => current.push_str(&unescape(text)),
            Node::Text(_) => {}
        }
    }
    strings
}

fn sheet_names(xml: &str) -> Vec<String> {
    Scanner::new(xml)
        .filter_map(|node| match node {
            Node::Open { name, attrs, .. } if local(name) == "sheet" => attr(attrs, "name").map(unescape),
            _ => None,
        })
        .collect()
}

/// For each cell style, whether it shows its number as a date. Excel stores a
/// date as a day count, and only the style says it is one.
fn date_styles(xml: &str) -> Vec<bool> {
    let mut custom: Vec<(u32, bool)> = Vec::new();
    let mut styles = Vec::new();
    let mut in_cell_xfs = false;
    for node in Scanner::new(xml) {
        match node {
            Node::Open { name, attrs, .. } => match local(name) {
                "numFmt" => {
                    let id = attr(attrs, "numFmtId").and_then(|v| v.parse().ok());
                    if let (Some(id), Some(code)) = (id, attr(attrs, "formatCode")) {
                        custom.push((id, is_date_format(&unescape(code))));
                    }
                }
                "cellXfs" => in_cell_xfs = true,
                "xf" if in_cell_xfs => {
                    let id: u32 = attr(attrs, "numFmtId").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let builtin = matches!(id, 14..=22 | 27..=36 | 45..=47 | 50..=58);
                    let is_date = custom.iter().find(|(c, _)| *c == id).map(|(_, d)| *d).unwrap_or(builtin);
                    styles.push(is_date);
                }
                _ => {}
            },
            Node::Close(name) if local(name) == "cellXfs" => in_cell_xfs = false,
            _ => {}
        }
    }
    styles
}

/// A number format is a date format when it has date letters outside quoted
/// text and outside brackets (colours, conditions, locale codes).
fn is_date_format(code: &str) -> bool {
    let mut quoted = false;
    let mut bracket = false;
    let mut escaped = false;
    for c in code.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' | '_' | '*' => escaped = true,
            '"' => quoted = !quoted,
            '[' if !quoted => bracket = true,
            ']' if !quoted => bracket = false,
            'd' | 'D' | 'm' | 'M' | 'y' | 'Y' | 'h' | 'H' | 's' | 'S' if !quoted && !bracket => return true,
            _ => {}
        }
    }
    false
}

/// Excel's day count as a date, with the time when there is one. Day 1 is
/// 1900-01-01 and Excel counts a 29 February 1900 that never was, which is why
/// the count starts from 1899-12-30.
fn serial_date(serial: f64) -> Option<String> {
    if !(1.0..2_958_466.0).contains(&serial) {
        return None;
    }
    let days = serial.floor() as i64;
    let seconds = ((serial - serial.floor()) * 86_400.0).round() as i64;
    // Days since 1970-01-01, then the civil-from-days algorithm.
    let z = days - 25_569 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let date = format!("{year:04}-{month:02}-{day:02}");
    if seconds == 0 || seconds >= 86_400 {
        return Some(date);
    }
    Some(format!("{date} {:02}:{:02}", seconds / 3_600, seconds % 3_600 / 60))
}

/// "B7" → 1: the column's position, counted from zero.
fn column_index(reference: &str) -> Option<usize> {
    let letters: Vec<u8> = reference.bytes().take_while(u8::is_ascii_alphabetic).collect();
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let n = letters.iter().fold(0usize, |n, b| n * 26 + usize::from(b.to_ascii_uppercase() - b'A') + 1);
    Some(n - 1)
}

fn sheet(xml: &str, strings: &[String], dates: &[bool]) -> String {
    let mut rows = String::new();
    let mut row: Vec<String> = Vec::new();
    // The cell being read: its column, type, style and value.
    let mut column = 0usize;
    let mut cell_type = "";
    let mut style = 0usize;
    let mut value = String::new();
    let mut in_value = false;

    for node in Scanner::new(xml) {
        match node {
            Node::Open { name, attrs, empty } => match local(name) {
                "row" => row.clear(),
                "c" => {
                    column = attr(attrs, "r").and_then(column_index).unwrap_or(row.len());
                    cell_type = attr(attrs, "t").unwrap_or("");
                    style = attr(attrs, "s").and_then(|v| v.parse().ok()).unwrap_or(0);
                    value.clear();
                }
                // `v` holds the value, `t` the text of an inline string.
                "v" | "t" => in_value = !empty,
                _ => {}
            },
            Node::Close(name) => match local(name) {
                "v" | "t" => in_value = false,
                "c" => {
                    let shown = match cell_type {
                        "s" => value.parse::<usize>().ok().and_then(|i| strings.get(i)).cloned().unwrap_or_default(),
                        "b" => if value == "1" { "TRUE".into() } else { "FALSE".into() },
                        "" | "n" if dates.get(style).copied().unwrap_or(false) => {
                            value.parse().ok().and_then(serial_date).unwrap_or_else(|| value.clone())
                        }
                        _ => value.clone(),
                    };
                    if !shown.is_empty() {
                        if row.len() <= column {
                            row.resize(column + 1, String::new());
                        }
                        // A cell is one field: its own tabs and line breaks become spaces.
                        row[column] = shown.replace(['\t', '\n', '\r'], " ");
                    }
                }
                "row" => {
                    if row.iter().any(|cell| !cell.is_empty()) {
                        rows.push_str(&row.join("\t"));
                        rows.push('\n');
                    }
                }
                _ => {}
            },
            Node::Text(text) if in_value => value.push_str(&unescape(text)),
            Node::Text(_) => {}
        }
    }
    rows
}

// ── A small XML scanner ───────────────────────────────────────────────────────
//
// Office writes plain, regular XML: tags, attributes, text and the five standard
// entities. That is all this reads — enough for these files, not a parser.

enum Node<'a> {
    Open { name: &'a str, attrs: &'a str, empty: bool },
    Close(&'a str),
    Text(&'a str),
}

struct Scanner<'a> {
    rest: &'a str,
}

impl<'a> Scanner<'a> {
    fn new(xml: &'a str) -> Self {
        Self { rest: xml }
    }
}

impl<'a> Iterator for Scanner<'a> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Node<'a>> {
        loop {
            if self.rest.is_empty() {
                return None;
            }
            if !self.rest.starts_with('<') {
                let end = self.rest.find('<').unwrap_or(self.rest.len());
                let (text, rest) = self.rest.split_at(end);
                self.rest = rest;
                return Some(Node::Text(text));
            }
            // Comments, declarations and processing instructions carry no text.
            let skip_to = if self.rest.starts_with("<!--") {
                Some("-->")
            } else if self.rest.starts_with("<?") {
                Some("?>")
            } else if self.rest.starts_with("<!") {
                Some(">")
            } else {
                None
            };
            if let Some(end) = skip_to {
                self.rest = self.rest.find(end).map(|i| &self.rest[i + end.len()..]).unwrap_or("");
                continue;
            }
            let Some(end) = self.rest.find('>') else {
                self.rest = "";
                return None;
            };
            let body = &self.rest[1..end];
            self.rest = &self.rest[end + 1..];
            if let Some(name) = body.strip_prefix('/') {
                return Some(Node::Close(name.trim()));
            }
            let (body, empty) = match body.strip_suffix('/') {
                Some(body) => (body, true),
                None => (body, false),
            };
            let split = body.find(char::is_whitespace).unwrap_or(body.len());
            return Some(Node::Open { name: &body[..split], attrs: &body[split..], empty });
        }
    }
}

/// `w:t` → `t`: the files' prefixes vary, the local names do not.
fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// The value of one attribute, still escaped.
fn attr<'a>(mut attrs: &'a str, key: &str) -> Option<&'a str> {
    loop {
        attrs = attrs.trim_start();
        let eq = attrs.find('=')?;
        let name = attrs[..eq].trim();
        let after = attrs[eq + 1..].trim_start();
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let close = after[1..].find(quote)?;
        let value = &after[1..1 + close];
        if name == key {
            return Some(value);
        }
        attrs = &after[close + 2..];
    }
}

fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(end) = rest.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .map(|hex| u32::from_str_radix(hex, 16))
                .or_else(|| entity.strip_prefix('#').map(str::parse))
                .and_then(Result::ok)
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn archive(parts: &[(&str, &str)]) -> Cursor<Vec<u8>> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, xml) in parts {
            zip.start_file(*name, stored).unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        let mut done = zip.finish().unwrap();
        done.set_position(0);
        done
    }

    #[test]
    fn a_word_document_comes_out_as_its_paragraphs() {
        let doc = archive(&[(
            "word/document.xml",
            r#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body>
               <w:p><w:r><w:t>Acta de reunión</w:t></w:r></w:p>
               <w:p><w:r><w:t xml:space="preserve">Costo: </w:t></w:r><w:r><w:t>Q 1,200 &amp; IVA</w:t></w:r></w:p>
               <w:p><w:r><w:t>A</w:t><w:tab/><w:t>B</w:t><w:br/><w:t>C</w:t></w:r></w:p>
               <w:p><w:r><w:instrText>PAGE</w:instrText></w:r></w:p>
               </w:body></w:document>"#,
        )]);
        let text = extract(doc, Kind::Word).unwrap();
        let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        assert_eq!(lines, ["Acta de reunión", "Costo: Q 1,200 & IVA", "A\tB", "C"]);
    }

    #[test]
    fn a_spreadsheet_comes_out_as_rows_with_dates_as_dates() {
        let book = archive(&[
            ("xl/workbook.xml", r#"<workbook><sheets><sheet name="Ventas &amp; cobros" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
            ("xl/sharedStrings.xml", r#"<sst><si><t>Cliente</t></si><si><r><t>Faro </t></r><r><t>Azul</t></r></si><si><t>Fecha</t></si></sst>"#),
            (
                "xl/styles.xml",
                r##"<styleSheet><numFmts><numFmt numFmtId="164" formatCode="dd/mm/yyyy"/><numFmt numFmtId="165" formatCode="#,##0.00 &quot;dias&quot;"/></numFmts>
                   <cellStyleXfs><xf numFmtId="14"/></cellStyleXfs>
                   <cellXfs><xf numFmtId="0"/><xf numFmtId="164"/><xf numFmtId="165"/></cellXfs></styleSheet>"##,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData>
                   <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>2</v></c><c r="D1" t="inlineStr"><is><t>Monto</t></is></c></row>
                   <row r="2"></row>
                   <row r="3"><c r="A3" t="s"><v>1</v></c><c r="B3" s="1"><v>46000</v></c><c r="D3" s="2"><f>SUM(X1:X2)</f><v>48250.5</v></c><c r="E3" t="b"><v>1</v></c></row>
                   </sheetData></worksheet>"#,
            ),
        ]);
        let text = extract(book, Kind::Excel).unwrap();
        assert_eq!(
            text,
            "--- Ventas & cobros ---\nCliente\tFecha\t\tMonto\nFaro Azul\t2025-12-09\t\t48250.5\tTRUE\n\n"
        );
    }

    #[test]
    fn slides_come_out_in_order() {
        let deck = archive(&[
            ("ppt/slides/slide10.xml", "<p:sld><a:p><a:r><a:t>Diez</a:t></a:r></a:p></p:sld>"),
            ("ppt/slides/slide2.xml", "<p:sld><a:p><a:r><a:t>Dos</a:t></a:r></a:p></p:sld>"),
            ("ppt/slides/_rels/slide2.xml.rels", "<Relationships/>"),
        ]);
        assert_eq!(extract(deck, Kind::PowerPoint).unwrap(), "--- Slide 1 ---\nDos\n--- Slide 2 ---\nDiez\n");
    }

    #[test]
    fn what_cannot_be_read_says_why() {
        assert!(extract(Cursor::new(b"not a zip".to_vec()), Kind::Word).is_err());
        assert!(extract(archive(&[("word/document.xml", "<w:document/>")]), Kind::Word).is_err());
        assert!(matches!(kind_of(Path::new("viejo.DOC")), Some(Err(_))));
        assert_eq!(kind_of(Path::new("Informe.DOCX")).unwrap().unwrap(), Kind::Word);
        assert!(kind_of(Path::new("notas.txt")).is_none());
    }

    #[test]
    fn date_formats_are_told_from_number_formats() {
        for code in ["dd/mm/yyyy", "[$-409]d-mmm-yy", "h:mm AM/PM", "yyyy\\-mm\\-dd"] {
            assert!(is_date_format(code), "{code} shows a date");
        }
        for code in ["0.00", "#,##0", "[Red]0.00", "0.00 \"dias\"", "0%", "@"] {
            assert!(!is_date_format(code), "{code} shows a number");
        }
    }

    #[test]
    fn day_counts_become_calendar_dates() {
        assert_eq!(serial_date(25_569.0).unwrap(), "1970-01-01");
        assert_eq!(serial_date(45_292.0).unwrap(), "2024-01-01");
        assert_eq!(serial_date(45_351.0).unwrap(), "2024-02-29");
        assert_eq!(serial_date(45_292.75).unwrap(), "2024-01-01 18:00");
        assert!(serial_date(0.5).is_none());
    }

    #[test]
    fn columns_and_entities() {
        assert_eq!(column_index("A1"), Some(0));
        assert_eq!(column_index("Z9"), Some(25));
        assert_eq!(column_index("AA10"), Some(26));
        assert_eq!(column_index("12"), None);
        assert_eq!(unescape("a &lt;b&gt; &#225; &#xE9; &unknown; & c"), "a <b> á é &unknown; & c");
        assert_eq!(attr(r#" spans="1:3" r="B2" t='s'"#, "r"), Some("B2"));
        assert_eq!(attr(r#" spans="1:3" r="B2" t='s'"#, "t"), Some("s"));
        assert_eq!(attr(r#" r="B2""#, "s"), None);
    }
}
