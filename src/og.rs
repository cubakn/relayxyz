use crate::config::Config;

const WIDTH: u32 = 1200;
const HEIGHT: u32 = 630;

const NIP_LABELS: &[(u32, &str)] = &[
    (1, "protocol"),
    (2, "follows"),
    (4, "DMs"),
    (9, "delete"),
    (11, "info"),
    (22, "comment"),
    (25, "react"),
    (40, "expiry"),
    (42, "auth"),
    (70, "protect"),
];

pub fn generate(config: &Config) -> Vec<u8> {
    let svg = build_svg(config);
    rasterize(&svg)
}

fn build_svg(config: &Config) -> String {
    let name = svg_escape(&config.name);
    let description = svg_escape(&config.description);

    let x = 120.0_f64;
    let content_w = 600.0_f64;
    let font = "Geist Mono, monospace";

    let pill_font = 18.0_f64;
    let char_w = 10.5_f64;
    let pill_px = 12.0_f64;
    let pill_py = 8.0_f64;
    let pill_h = pill_font + 2.0 * pill_py;
    let gap_x = 9.0_f64;
    let gap_y = 10.0_f64;
    let pills_y = 355.0_f64;

    let mut pills = String::new();
    let mut cx = 0.0_f64;
    let mut cy = 0.0_f64;

    for &(num, label) in NIP_LABELS {
        let text_len = format!("{:02} {}", num, label).len();
        let text_w = text_len as f64 * char_w;
        let pill_w = text_w + 2.0 * pill_px;

        if cx > 0.0 && cx + pill_w > content_w {
            cx = 0.0;
            cy += pill_h + gap_y;
        }

        let px = x + cx;
        let py = pills_y + cy;
        let tx = px + pill_px;
        let ty = py + pill_py + pill_font * 0.78;
        let num_str = format!("{:02}", num);
        let lx = tx + 3.0 * char_w;

        pills.push_str(&format!(
            r##"<rect x="{px}" y="{py}" width="{pw}" height="{ph}" rx="4" fill="none" stroke="#fafafa" stroke-opacity="0.12" stroke-width="1"/><text x="{tx}" y="{ty}" font-family="{f}" font-size="{fs}" fill="#f7931a">{n}</text><text x="{lx}" y="{ty}" font-family="{f}" font-size="{fs}" fill="#fafafa">{l}</text>"##,
            px = px,
            py = py,
            pw = pill_w,
            ph = pill_h,
            tx = tx,
            ty = ty,
            lx = lx,
            f = font,
            fs = pill_font,
            n = num_str,
            l = label,
        ));

        cx += pill_w + gap_x;
    }

    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}"><rect width="{w}" height="{h}" fill="#0a0a0a"/><text x="{x}" y="205" font-family="{f}" font-size="52" font-weight="700" fill="#fafafa">{name}</text><text x="{x}" y="250" font-family="{f}" font-size="24" fill="#fafafa" fill-opacity="0.5">{desc}</text><text x="{x}" y="330" font-family="{f}" font-size="14" fill="#fafafa" fill-opacity="0.35" letter-spacing="1.4">SUPPORTED NIPS</text>{pills}</svg>"##,
        w = WIDTH,
        h = HEIGHT,
        x = x,
        f = font,
        name = name,
        desc = description,
        pills = pills,
    )
}

fn rasterize(svg: &str) -> Vec<u8> {
    let mut fontdb = resvg::usvg::fontdb::Database::new();

    for path in ["public/GeistMono-Regular.ttf", "public/GeistMono-Bold.ttf"] {
        match std::fs::read(path) {
            Ok(data) => {
                let before = fontdb.len();
                fontdb.load_font_data(data);
                if fontdb.len() == before {
                    eprintln!("warning: failed to load font from {path}");
                }
            }
            Err(e) => eprintln!("warning: could not read {path}: {e}"),
        }
    }
    fontdb.set_monospace_family("Geist Mono");

    let options = resvg::usvg::Options {
        fontdb: std::sync::Arc::new(fontdb),
        ..Default::default()
    };

    let tree = resvg::usvg::Tree::from_str(svg, &options).expect("invalid OG SVG");
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(WIDTH, HEIGHT).expect("OG pixmap allocation failed");
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::default(),
        &mut pixmap.as_mut(),
    );

    pixmap.encode_png().expect("OG PNG encode failed")
}

fn svg_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
