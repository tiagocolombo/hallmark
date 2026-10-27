//! A Gazette page as HTML: one file, the stylesheet inlined, no scripts.
//! Every piece of text is escaped here; the diagram is laid out here too.

use std::collections::HashMap;
use std::fmt::Write as _;

use super::Diagram;
use super::page::{Page, Plate};
use crate::findings::{Counts, Severity};

const CSS: &str = include_str!("gazette.css");

/// Lessons shown in full; the rest are listed by ID.
const LESSONS: usize = 6;

/// Escapes text for HTML, in content and in attributes.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// Escaped text, with `code` in backticks set in the mono face.
fn prose(s: &str) -> String {
    let mut o = String::new();
    for (i, part) in s.split('`').enumerate() {
        if i % 2 == 1 && !part.is_empty() {
            let _ = write!(o, "<span class=\"mono\">{}</span>", esc(part));
        } else {
            o.push_str(&esc(part));
        }
    }
    o
}

/// Standard base64, for inlining images.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        o.push(T[(n >> 18) as usize & 63] as char);
        o.push(T[(n >> 12) as usize & 63] as char);
        o.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        o.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    o
}

fn words(n: u32) -> String {
    const W: [&str; 11] = [
        "No", "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten",
    ];
    W.get(n as usize)
        .map(|w| w.to_string())
        .unwrap_or_else(|| n.to_string())
}

fn usd(x: f64) -> String {
    format!("${x:.2}")
}

fn sev_class(s: Severity) -> &'static str {
    match s {
        Severity::Blocker | Severity::Major => "M",
        Severity::Minor => "m",
        Severity::Nit => "t",
    }
}

fn sev_word(s: Severity) -> &'static str {
    match s {
        Severity::Blocker => "Blocker",
        Severity::Major => "Major",
        Severity::Minor => "Minor",
        Severity::Nit => "Nit",
    }
}

fn squares(c: &Counts) -> String {
    let mut s = String::new();
    for (n, class) in [(c.blocker + c.major, "M"), (c.minor, "m"), (c.nit, "t")] {
        for _ in 0..n.min(40) {
            let _ = write!(s, "<i class=\"{class}\"></i>");
        }
    }
    if s.is_empty() {
        s.push_str("<i class=\"z\">nothing open</i>");
    }
    s
}

/// The head line that says which hall printed a page.
pub fn generator() -> String {
    format!(
        "<meta name=\"generator\" content=\"hall {}\">",
        env!("HALL_VERSION")
    )
}

/// The head line that says how many journal events a page was printed from.
pub fn events_meta(n: usize) -> String {
    format!("<meta name=\"hall-events\" content=\"{n}\">")
}

/// The whole page.
pub fn render(p: &Page) -> String {
    let mut h = String::with_capacity(32 * 1024);
    let _ = write!(
        h,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\n\
{generator}\n{events}\n\
<title>Hallmark Gazette {id}</title>\n<style>\n{CSS}</style>\n</head>\n<body>\n<div class=\"sheet\">\n",
        generator = generator(),
        events = events_meta(p.events),
        id = esc(&p.id),
    );
    masthead(&mut h, p);
    h.push_str("<div class=\"front\">\n");
    lead(&mut h, p);
    side(&mut h, p);
    h.push_str("</div>\n");
    // The diagram gets the whole width: a flow reads left to right.
    if let Some(d) = &p.diagram
        && let Some(svg) = diagram(d)
    {
        let _ = writeln!(
            h,
            "<figure class=\"plate wide\"><div class=\"frame dia\">{svg}</div><figcaption><b>Plate {}.</b> {}</figcaption></figure>",
            roman(p.plates.len() + 1),
            prose(if d.caption.trim().is_empty() {
                "How it fits together."
            } else {
                d.caption.trim()
            })
        );
    }
    caught(&mut h, p);
    lower(&mut h, p);
    ledger(&mut h, p);
    if !p.dispatch.is_empty() {
        h.push_str("<div class=\"dispatch\">");
        for (label, big, small) in &p.dispatch {
            let _ = write!(
                h,
                "<div><span class=\"label\">{}</span><b>{}</b>{}</div>",
                esc(label),
                esc(big),
                esc(small)
            );
        }
        h.push_str("</div>\n");
    }
    let _ = write!(
        h,
        "<footer class=\"colophon\"><span>{} from {} journal events · <span class=\"mono\">hall log {}</span></span>\
<span>Printed by hall {}</span></footer>\n</div>\n</body>\n</html>\n",
        if p.by_librarian {
            "Written by the librarian"
        } else {
            "Set from the item's own words"
        },
        p.events,
        esc(&p.id),
        esc(env!("HALL_VERSION")),
    );
    h
}

fn masthead(h: &mut String, p: &Page) {
    let _ = write!(
        h,
        "<header class=\"mast\">\n<div class=\"ear\"><b>Edition</b><span class=\"big\">{id}</span><br>{repo} · <span class=\"mono\">{base}</span></div>\n\
<div class=\"title\">The Hallmark <span>Gazette</span></div>\n\
<div class=\"ear r\"><b>Price</b><span class=\"big\">{cost}</span><br>in model time, all hands</div>\n</header>\n\
<div class=\"dateline\"><span>{date}</span><span>{outcome}</span>{took}<span>{rounds} round{s} of review</span></div>\n",
        id = esc(&p.id),
        repo = esc(&p.repo),
        base = esc(&p.base),
        cost = usd(p.cost),
        date = esc(&p.date),
        outcome = esc(&p.outcome),
        took = p
            .took
            .as_deref()
            .map(|t| format!("<span>{}</span>", esc(t)))
            .unwrap_or_default(),
        rounds = words(p.rounds),
        s = if p.rounds == 1 { "" } else { "s" },
    );
}

fn stamp(sha: &str) -> String {
    format!(
        "<svg class=\"stamp\" width=\"96\" height=\"40\" viewBox=\"0 0 96 40\" role=\"img\" aria-label=\"Hallmarked at commit {sha}\">\
<rect x=\"1.5\" y=\"1.5\" width=\"93\" height=\"37\" rx=\"10\" fill=\"none\" stroke=\"var(--brass)\" stroke-width=\"2\"/>\
<rect x=\"5.5\" y=\"5.5\" width=\"85\" height=\"29\" rx=\"7\" fill=\"none\" stroke=\"var(--brass)\" stroke-width=\"0.8\"/>\
<text x=\"48\" y=\"18\" text-anchor=\"middle\" font-size=\"9\" letter-spacing=\"2\" fill=\"var(--brass)\">HALLMARKED</text>\
<text x=\"48\" y=\"29\" text-anchor=\"middle\" font-size=\"9.5\" fill=\"var(--brass)\">{sha}</text></svg>",
        sha = esc(sha)
    )
}

fn lead(h: &mut String, p: &Page) {
    h.push_str("<article class=\"lead\">\n");
    if !p.kicker.is_empty() {
        let _ = writeln!(h, "<div class=\"kicker\">{}</div>", esc(&p.kicker));
    }
    let _ = writeln!(h, "<h1>{}</h1>", prose(&p.headline));
    if !p.deck.is_empty() {
        let _ = writeln!(h, "<p class=\"deck\">{}</p>", prose(&p.deck));
    }
    let _ = writeln!(
        h,
        "<div class=\"byline\">{}<span>{}</span></div>",
        p.stamp.as_deref().map(stamp).unwrap_or_default(),
        esc(&p.byline)
    );
    if !p.story.is_empty() {
        h.push_str("<div class=\"cols2\">");
        for (i, para) in p.story.iter().enumerate() {
            let drop = if i == 0 && para.chars().count() > 120 {
                " class=\"drop\""
            } else {
                ""
            };
            let _ = write!(h, "<p{drop}>{}</p>", prose(para));
        }
        h.push_str("</div>\n");
    }
    if !p.links.is_empty() {
        h.push_str("<ul class=\"links\">");
        for (url, caption) in &p.links {
            let _ = write!(
                h,
                "<li><a href=\"{u}\">{u}</a>{c}</li>",
                u = esc(url),
                c = caption
                    .as_deref()
                    .map(|c| format!(" <span>· {}</span>", esc(c)))
                    .unwrap_or_default()
            );
        }
        h.push_str("</ul>\n");
    }
    for (i, plate) in p.plates.iter().enumerate() {
        plate_html(h, plate, i + 1);
    }
    h.push_str("</article>\n");
}

fn plate_html(h: &mut String, plate: &Plate, n: usize) {
    let caption = plate.caption.as_deref().map(prose).unwrap_or_default();
    if plate.image {
        let _ = writeln!(
            h,
            "<figure class=\"plate\"><div class=\"frame\"><img src=\"{}\" alt=\"{}\"></div><figcaption><b>Plate {}.</b> {caption}</figcaption></figure>",
            esc(&plate.src),
            esc(plate.caption.as_deref().unwrap_or("An attachment")),
            roman(n)
        );
    } else {
        let name = plate.src.rsplit('/').next().unwrap_or(&plate.src);
        let _ = writeln!(
            h,
            "<figure class=\"plate\"><div class=\"frame\"><a href=\"{}\">{}</a></div><figcaption><b>Plate {}.</b> {caption}</figcaption></figure>",
            esc(&plate.src),
            esc(name),
            roman(n)
        );
    }
}

fn roman(n: usize) -> String {
    const R: [(usize, &str); 7] = [
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut n = n.min(89);
    let mut s = String::new();
    for (v, r) in R {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

fn side(h: &mut String, p: &Page) {
    h.push_str("<aside class=\"side\">\n");
    if !p.verdicts.is_empty() {
        h.push_str("<section><h2>The verdicts, round by round</h2><div class=\"rounds\">");
        for v in &p.verdicts {
            let _ = write!(
                h,
                "<div class=\"round\"><span class=\"n\">{}</span><div class=\"sq\">{}</div><small>{}</small></div>",
                v.round,
                squares(&v.counts),
                prose(&v.line)
            );
        }
        h.push_str(
            "</div><div class=\"key\"><span><span class=\"sq\"><i class=\"M\"></i></span>major</span>\
<span><span class=\"sq\"><i class=\"m\"></i></span>minor</span>\
<span><span class=\"sq\"><i class=\"t\"></i></span>nit</span><span>open at each verdict</span></div></section>\n",
        );
    }
    if !p.checks.is_empty() || p.ci.is_some() {
        let failed: u32 = p.checks.iter().map(|c| c.ran - c.passed).sum();
        let (title, said) = if failed == 0 {
            (
                "Checks: clear all along",
                "Every check passed in every round.".to_string(),
            )
        } else {
            (
                "Checks: some rain",
                format!(
                    "{failed} check run{} failed along the way.",
                    if failed == 1 { "" } else { "s" }
                ),
            )
        };
        let _ = write!(
            h,
            "<section class=\"weather\"><h3>{title}</h3>{}<ul>",
            esc(&said)
        );
        for c in &p.checks {
            let _ = write!(
                h,
                "<li><span>{}</span><span{}>{} of {}</span></li>",
                esc(&c.name),
                if c.passed < c.ran {
                    " class=\"bad\""
                } else {
                    ""
                },
                c.passed,
                c.ran
            );
        }
        if let Some(ci) = p.ci {
            let _ = write!(
                h,
                "<li><span>CI on the pull request</span><span{}>{}</span></li>",
                if ci { "" } else { " class=\"bad\"" },
                if ci { "passed" } else { "failed" }
            );
        }
        h.push_str("</ul></section>\n");
    }
    if !p.brief.is_empty() {
        let wide = p.brief.iter().any(|(t, _)| t.len() > 5);
        let _ = write!(
            h,
            "<section><h2>The day in brief</h2><ol class=\"brief\"{}>",
            if wide { " style=\"--when:96px\"" } else { "" }
        );
        for (t, text) in &p.brief {
            let _ = write!(
                h,
                "<li><time>{}</time><span>{}</span></li>",
                esc(t),
                prose(text)
            );
        }
        h.push_str(
            "</ol><div class=\"label\" style=\"margin-top:8px\">Times in UTC</div></section>\n",
        );
    }
    h.push_str("</aside>\n");
}

fn caught(h: &mut String, p: &Page) {
    if p.caught.is_empty() {
        return;
    }
    let total: usize = p.caught.iter().map(|d| d.stories.len()).sum();
    let _ = write!(
        h,
        "<section class=\"section\"><header><h2>What the reviewer caught</h2><span class=\"label\">{} settled · {} still open</span></header><div class=\"caught\">",
        total,
        p.corrections.len()
    );
    for d in &p.caught {
        let _ = write!(h, "<div class=\"desk\"><h3>{}</h3>", esc(&d.name));
        for s in &d.stories {
            let _ = write!(
                h,
                "<div class=\"story\"><h4>{}</h4>{}<div class=\"tag\"><span class=\"sev {}\">{}</span><span class=\"{}\">{}</span><span class=\"mono\">{}</span></div></div>",
                prose(&s.headline),
                if s.text.is_empty() {
                    String::new()
                } else {
                    format!("<p>{}</p>", prose(&s.text))
                },
                sev_class(s.severity),
                sev_word(s.severity),
                if s.outcome.starts_with("Fixed") {
                    "ok"
                } else {
                    ""
                },
                esc(&s.outcome),
                esc(&s.id)
            );
        }
        h.push_str("</div>");
    }
    h.push_str("</div></section>\n");
}

fn lower(h: &mut String, p: &Page) {
    if p.corrections.is_empty() && p.lessons.is_empty() {
        return;
    }
    h.push_str("<div class=\"lower\">");
    if !p.corrections.is_empty() {
        let worst = p
            .corrections
            .iter()
            .filter(|c| !c.pre_existing)
            .map(|c| c.severity)
            .min_by_key(|s| match s {
                Severity::Blocker => 0,
                Severity::Major => 1,
                Severity::Minor => 2,
                Severity::Nit => 3,
            });
        let note = match worst {
            Some(Severity::Blocker | Severity::Major) => {
                "These were still open when the item stopped. Each one is a candidate for a follow-up item."
            }
            _ if p.corrections.iter().any(|c| c.pre_existing) => {
                "Some were there before the change and never blocked it. Each one is a candidate for a follow-up item."
            }
            _ => {
                "These were small enough that the item could ship with them open. Each one is a candidate for a follow-up item."
            }
        };
        let _ = write!(
            h,
            "<section class=\"section\"><header><h2>Corrections</h2><span class=\"label\">Still open</span></header><div class=\"corr\"><p>{note}</p><ol>"
        );
        for c in &p.corrections {
            let _ = write!(
                h,
                "<li>{} <span class=\"mono\">{}</span>{}</li>",
                prose(&c.text),
                esc(&c.id),
                if c.pre_existing {
                    " <span class=\"sev\">Pre-existing</span>".to_string()
                } else if matches!(c.severity, Severity::Blocker | Severity::Major) {
                    format!(" <span class=\"sev M\">{}</span>", sev_word(c.severity))
                } else {
                    String::new()
                }
            );
        }
        h.push_str("</ol></div></section>");
    }
    if !p.lessons.is_empty() {
        let _ = write!(
            h,
            "<section class=\"section\"><header><h2>From the Library</h2><span class=\"label\">{} lesson{}</span></header><div class=\"library\">",
            p.lessons.len(),
            if p.lessons.len() == 1 { "" } else { "s" }
        );
        for l in p.lessons.iter().take(LESSONS) {
            let _ = write!(
                h,
                "<div class=\"lesson\"><span class=\"id\">{}</span><p>{}</p><small>{}</small></div>",
                esc(&l.id),
                prose(&l.title),
                esc(&l.note)
            );
        }
        if p.lessons.len() > LESSONS {
            let rest: Vec<&str> = p.lessons[LESSONS..].iter().map(|l| l.id.as_str()).collect();
            let _ = write!(
                h,
                "<p class=\"lesson-more\">And {} more: <span class=\"mono\">{}</span></p>",
                rest.len(),
                esc(&rest.join(", "))
            );
        }
        if p.lessons
            .iter()
            .any(|l| l.note.ends_with("waiting for you"))
        {
            h.push_str("<p class=\"label\" style=\"margin:0\">Review them with <span class=\"mono\" style=\"text-transform:none;letter-spacing:0\">hall library review</span></p>");
        }
        h.push_str("</div></section>");
    }
    h.push_str("</div>\n");
}

fn ledger(h: &mut String, p: &Page) {
    if p.ledger.is_empty() {
        return;
    }
    let max = p.ledger.iter().map(|r| r.cost).fold(0.0_f64, f64::max);
    h.push_str(
        "<section class=\"section\"><header><h2>The ledger</h2><span class=\"label\">What this edition cost</span></header>\
<div class=\"ledger-wrap\"><table><thead><tr><th>Who</th><th>Model</th><th>What they did</th><th class=\"num\">Cost</th><th style=\"width:28%\"></th></tr></thead><tbody>",
    );
    for r in &p.ledger {
        let w = if max > 0.0 { r.cost / max * 100.0 } else { 0.0 };
        let _ = write!(
            h,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"num\">{}</td><td><span class=\"bar\" style=\"width:{w:.1}%\"></span></td></tr>",
            esc(&r.who),
            esc(&r.model),
            esc(&r.did),
            usd(r.cost)
        );
    }
    let _ = writeln!(
        h,
        "</tbody><tfoot><tr><td colspan=\"3\">Total</td><td class=\"num\">{}</td><td></td></tr></tfoot></table></div></section>",
        usd(p.cost)
    );
}

const BOX_W: i32 = 136;
const BOX_H: i32 = 58;
const GAP_X: i32 = 110;
const GAP_Y: i32 = 30;
const TOP: i32 = 30;
const LEFT: i32 = 6;

fn clip_to(s: &str, n: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= n {
        t.to_string()
    } else {
        let mut c: String = t.chars().take(n - 1).collect();
        c.push('…');
        c
    }
}

/// Lays the outline out left to right and draws it, or None when it has
/// fewer than two nodes. Nodes take columns by the longest path to them
/// along edges that point forward in the outline's order; an edge that
/// points back is drawn dashed, under the boxes.
pub fn diagram(d: &Diagram) -> Option<String> {
    let mut ids: Vec<&str> = Vec::new();
    let mut nodes = Vec::new();
    for n in &d.nodes {
        let id = n.id.trim();
        if id.is_empty() || ids.contains(&id) || nodes.len() == 10 {
            continue;
        }
        ids.push(id);
        nodes.push(n);
    }
    if nodes.len() < 2 {
        return None;
    }
    let at: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let edges: Vec<(usize, usize, &str)> = d
        .edges
        .iter()
        .filter_map(|e| {
            let (a, b) = (*at.get(e.from.trim())?, *at.get(e.to.trim())?);
            (a != b).then_some((a, b, e.label.trim()))
        })
        .take(16)
        .collect();

    let n = nodes.len();
    let mut rank = vec![0usize; n];
    for _ in 0..n {
        for &(a, b, _) in &edges {
            if a < b && rank[b] < rank[a] + 1 {
                rank[b] = rank[a] + 1;
            }
        }
    }
    let cols = rank.iter().max().map_or(1, |m| m + 1);
    let mut row = vec![0usize; n];
    let mut per_col = vec![0usize; cols];
    for i in 0..n {
        row[i] = per_col[rank[i]];
        per_col[rank[i]] += 1;
    }
    let rows = per_col.iter().copied().max().unwrap_or(1) as i32;
    let x = |i: usize| LEFT + rank[i] as i32 * (BOX_W + GAP_X);
    let y = |i: usize| TOP + row[i] as i32 * (BOX_H + GAP_Y);
    let bottom = TOP + rows * (BOX_H + GAP_Y) - GAP_Y;
    let back: Vec<&(usize, usize, &str)> = edges.iter().filter(|(a, b, _)| a > b).collect();
    let width = LEFT * 2 + cols as i32 * (BOX_W + GAP_X) - GAP_X;
    let height = bottom
        + if back.is_empty() {
            10
        } else {
            18 + back.len() as i32 * 26
        };

    let mut s = String::new();
    let _ = write!(
        s,
        "<svg viewBox=\"0 0 {width} {height}\" style=\"max-width:{width}px;min-width:min({width}px,560px)\" role=\"img\" aria-label=\"{}\">\
<defs><marker id=\"ah\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"7\" markerHeight=\"7\" orient=\"auto-start-reverse\">\
<path class=\"head\" d=\"M0,0 L10,5 L0,10 z\"/></marker></defs>",
        esc(&format!("Diagram: {}", d.caption.trim()))
    );
    for (i, node) in nodes.iter().enumerate() {
        let (x0, y0) = (x(i), y(i));
        let cx = x0 + BOX_W / 2;
        let note = node.note.trim();
        let _ = write!(
            s,
            "<g class=\"node\"><rect x=\"{x0}\" y=\"{y0}\" width=\"{BOX_W}\" height=\"{BOX_H}\"/>"
        );
        if note.is_empty() {
            let _ = write!(
                s,
                "<text x=\"{cx}\" y=\"{}\">{}</text>",
                y0 + BOX_H / 2 + 5,
                esc(&clip_to(&node.label, 17))
            );
        } else {
            let _ = write!(
                s,
                "<text x=\"{cx}\" y=\"{}\">{}</text><text class=\"s\" x=\"{cx}\" y=\"{}\">{}</text>",
                y0 + 25,
                esc(&clip_to(&node.label, 17)),
                y0 + 43,
                esc(&clip_to(note, 22))
            );
        }
        s.push_str("</g>");
    }
    let mut depth = 0;
    for &(a, b, label) in &edges {
        let label = esc(&clip_to(label, 18));
        if a < b {
            let (x1, y1) = (x(a) + BOX_W, y(a) + BOX_H / 2);
            let (x2, y2) = (x(b), y(b) + BOX_H / 2);
            if rank[b] - rank[a] > 1 && y1 == y2 {
                // Over the boxes in between.
                let (ax, bx) = (x(a) + BOX_W / 2, x(b) + BOX_W / 2);
                let top = y(a) - 12;
                let _ = write!(
                    s,
                    "<path class=\"edge\" d=\"M{ax},{} V{top} H{bx} V{}\" marker-end=\"url(#ah)\"/>",
                    y(a),
                    y(b) - 2
                );
                if !label.is_empty() {
                    let _ = write!(
                        s,
                        "<text class=\"e\" x=\"{}\" y=\"{}\">{label}</text>",
                        (ax + bx) / 2,
                        top - 5
                    );
                }
            } else {
                let mid = x1 + (x2 - x1) / 2;
                let d = if y1 == y2 {
                    format!("M{x1},{y1} H{}", x2 - 2)
                } else {
                    format!("M{x1},{y1} H{mid} V{y2} H{}", x2 - 2)
                };
                let _ = write!(
                    s,
                    "<path class=\"edge\" d=\"{d}\" marker-end=\"url(#ah)\"/>"
                );
                if !label.is_empty() {
                    let _ = write!(
                        s,
                        "<text class=\"e\" x=\"{mid}\" y=\"{}\">{label}</text>",
                        y1.min(y2) - 8
                    );
                }
            }
        } else {
            depth += 1;
            let line = bottom + depth * 26;
            let (ax, bx) = (x(a) + BOX_W / 2, x(b) + BOX_W / 2);
            let _ = write!(
                s,
                "<path class=\"edge back\" d=\"M{ax},{} V{line} H{bx} V{}\" marker-end=\"url(#ah)\"/>",
                y(a) + BOX_H,
                y(b) + BOX_H + 2
            );
            if !label.is_empty() {
                let _ = write!(
                    s,
                    "<text class=\"e\" x=\"{}\" y=\"{}\">{label}</text>",
                    (ax + bx) / 2,
                    line - 5
                );
            }
        }
    }
    s.push_str("</svg>");
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gazette::{Edge, Node};

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn text_is_escaped_and_code_is_set_in_mono() {
        assert_eq!(
            prose("run `hall <x>` & \"go\""),
            "run <span class=\"mono\">hall &lt;x&gt;</span> &amp; &quot;go&quot;"
        );
    }

    #[test]
    fn numerals() {
        assert_eq!(roman(1), "I");
        assert_eq!(roman(4), "IV");
        assert_eq!(roman(14), "XIV");
        assert_eq!(words(5), "Five");
        assert_eq!(words(12), "12");
    }

    fn node(id: &str) -> Node {
        Node {
            id: id.into(),
            label: id.to_uppercase(),
            note: String::new(),
        }
    }

    fn edge(from: &str, to: &str) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            label: format!("{from}→{to}"),
        }
    }

    #[test]
    fn a_diagram_needs_two_nodes() {
        let d = Diagram {
            nodes: vec![node("a")],
            ..Diagram::default()
        };
        assert!(diagram(&d).is_none());
    }

    #[test]
    fn diagrams_lay_out_left_to_right_and_draw_back_edges_dashed() {
        let d = Diagram {
            caption: "c".into(),
            nodes: vec![node("a"), node("b"), node("c"), node("a")],
            edges: vec![
                edge("a", "b"),
                edge("b", "c"),
                edge("c", "a"),
                edge("a", "zz"),
            ],
        };
        let svg = diagram(&d).unwrap();
        // Three distinct nodes in three columns; the duplicate and the
        // dangling edge are dropped.
        assert_eq!(svg.matches("<rect").count(), 3);
        assert!(svg.contains("x=\"252\""), "{svg}");
        assert!(svg.contains("x=\"498\""), "{svg}");
        assert_eq!(svg.matches("class=\"edge\"").count(), 2);
        assert_eq!(svg.matches("class=\"edge back\"").count(), 1);
        assert!(svg.contains("a→b"));
        assert!(!svg.contains("zz"));
    }

    #[test]
    fn an_open_pre_existing_blocker_is_not_called_small() {
        use crate::gazette::page::Correction;
        let mut p = Page::default();
        p.corrections.push(Correction {
            text: "An old race.".into(),
            id: "F1".into(),
            severity: Severity::Blocker,
            pre_existing: true,
        });
        let mut h = String::new();
        lower(&mut h, &p);
        assert!(h.contains("there before the change and never blocked it"), "{h}");
        assert!(!h.contains("small enough"), "{h}");
        assert!(!h.contains("still open when the item stopped"), "{h}");
        assert!(h.contains("Pre-existing"), "{h}");
    }

    #[test]
    fn labels_are_escaped_in_the_diagram() {
        let d = Diagram {
            caption: "<script>".into(),
            nodes: vec![
                Node {
                    id: "a".into(),
                    label: "<b>".into(),
                    note: "\"q\"".into(),
                },
                node("b"),
            ],
            edges: vec![edge("a", "b")],
        };
        let svg = diagram(&d).unwrap();
        assert!(!svg.contains("<b>"));
        assert!(!svg.contains("<script>"));
        assert!(svg.contains("&lt;b&gt;"));
    }
}
