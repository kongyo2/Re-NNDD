//! ニコニコ風コメントを ASS (Advanced SubStation Alpha) 字幕へ変換するコア。
//!
//! Phase 1.9「コメント焼き込みエクスポート」の心臓部。プレイヤーが
//! `@xpadev-net/niconicomments` で Canvas にリアルタイム描画しているものを、
//! ffmpeg の `ass` フィルタで焼き込めるように **静的な ASS** へ落とし込む。
//!
//! # 設計: 本家 niconicomments の HTML5 レンダラを忠実移植
//!
//! 旧実装は「レーングリッド + 単純な時間衝突」という近似だったため、本家との
//! 配置ズレが大きかった。本実装は `niconicomments` (v0.2.78) の HTML5 描画
//! アルゴリズムを Rust へ**そのまま移植**する:
//!
//! - 全計算を本家と同じ **1920×1080 の内部座標系**で行い、ASS の PlayResX/Y も
//!   1920×1080 に固定する。libass が出力解像度へスケールするので、本家の
//!   `setScale(width/1920, height/1080)` と等価な見た目になる(解像度非依存)。
//! - フォントサイズは `getCharSize` / `getLineHeight` / `getFontSizeAndScale` を
//!   忠実移植 (small/medium/big = 1080 換算で char 箱 65/95.5/138px, 字面
//!   50.6/75.9/109.6px)。改行リサイズ・横幅オーバーフローリサイズも再現。
//! - 流れる (naka) コメントは本家の速度式
//!   `(commentDrawRange + width*0.95) / (long+100)` と `getPosX` をそのまま使い、
//!   ASS の `\move` で線形補間する (幅依存の速度・滞留時間を完全再現)。
//! - 当たり判定は本家の `processMovableComment` / `getMovablePosY` /
//!   `processFixedComment` / `getFixedPosY` / `getPosY` を移植。vpos スロット
//!   ごとの衝突配列・投稿者/視聴者レイヤー分離・画面溢れ時のランダム配置まで
//!   再現する。
//! - 文字色 (通常 + プレミアム 2 系列 + #hex)、縁取り (黒 40% / 黒文字は白縁)、
//!   nico:stroke/fill/opacity、`@N` 長さ、ender、invisible に対応。
//!
//! ここは I/O も ffmpeg も触らない純粋ロジック。文字列 ASS を返すだけなので
//! 単体テストで全分岐を固定できる (README「Rust コア重視」)。

use std::collections::HashMap;

// ── 本家 niconicomments の config (HTML5) 由来の定数 ──────────────────────

/// 内部キャンバス幅 (本家 config.canvasWidth)。
const CANVAS_WIDTH: f64 = 1920.0;
/// 内部キャンバス高 (本家 config.canvasHeight)。
const CANVAS_HEIGHT: f64 = 1080.0;
/// 683 ステージ → 1920 キャンバスへのスケール (本家 commentScale.html5 = 1920/683)。
const COMMENT_SCALE: f64 = CANVAS_WIDTH / 683.0;
/// コメント処理範囲 (本家 commentDrawRange)。
const DRAW_RANGE: f64 = 1530.0;
/// 処理範囲外パディング (本家 commentDrawPadding)。
const DRAW_PADDING: f64 = 195.0;
/// 当たり判定の左端 (本家 collisionRange.left)。
const COLLISION_LEFT: f64 = 235.0;
/// 当たり判定の右端 (本家 collisionRange.right)。
const COLLISION_RIGHT: f64 = 1685.0;
/// コメント間の横の余白 (本家 collisionPadding)。
const COLLISION_PADDING: f64 = 5.0;
/// naka コメントの速度補正 (本家 nakaCommentSpeedOffset)。
const SPEED_OFFSET: f64 = 0.95;
/// 描画範囲 (683 ステージ; 本家 commentStageSize.html5)。横幅オーバーフロー判定用。
const STAGE_WIDTH: f64 = 512.0;
const STAGE_FULL_WIDTH: f64 = 683.0;
const STAGE_HEIGHT: f64 = 384.0;
/// 縁取り線の太さ (本家 contextLineWidth.html5)。
const CONTEXT_LINE_WIDTH: f64 = 2.8;
/// 縁取りの不透明度 (本家 contextStrokeOpacity)。
const STROKE_OPACITY: f64 = 0.4;
/// 字面サイズの下限 (本家 html5MinFontSize)。
const MIN_FONT_SIZE: f64 = 10.0;
/// コメント既定の長さ (vpos=センチ秒; 本家 long 既定 300 = 3 秒)。
const DEFAULT_LONG_CS: i64 = 300;

/// 各サイズの doubleResized 行数 (本家 html5LineCounts.doubleResized)。
fn double_resized(size: Size) -> f64 {
    match size {
        Size::Big => 7.8,
        Size::Medium => 11.3,
        Size::Small => 16.6,
    }
}
/// 各サイズの default 行数 (本家 html5LineCounts.default)。
fn default_line_count(size: Size) -> f64 {
    match size {
        Size::Big => 8.4,
        Size::Medium => 13.1,
        Size::Small => 21.0,
    }
}
/// 各サイズの resized 行数 (本家 html5LineCounts.resized)。
fn resized_line_count(size: Size) -> f64 {
    match size {
        Size::Big => 16.0,
        Size::Medium => 25.4,
        Size::Small => 38.0,
    }
}
/// 改行リサイズが発生する行数 (本家 lineBreakCount)。
fn line_break_count(size: Size) -> usize {
    match size {
        Size::Big => 3,
        Size::Medium => 5,
        Size::Small => 7,
    }
}

// ── 入力構造体 ─────────────────────────────────────────────────────────

/// 焼き込み対象 1 コメント。DB / API どちらの形からでも詰められる薄い構造体。
#[derive(Debug, Clone)]
pub struct BurnInComment {
    /// 再生位置 (ミリ秒)。
    pub vpos_ms: i64,
    /// 本文。
    pub content: String,
    /// niconico コマンド列 (mail をスペース分割したもの)。色・位置・サイズ。
    pub commands: Vec<String>,
    /// 投稿者コメントか。当たり判定のレイヤー分離に使う。
    pub is_owner: bool,
}

/// ASS 生成オプション。UI から調整できる項目はここに集約する。
#[derive(Debug, Clone)]
pub struct AssOptions {
    /// 出力解像度 (= 入力動画解像度)。ASS は 1920×1080 固定で出力し libass が
    /// スケールするため、現状レイアウト計算には未使用 (将来用に保持)。
    pub width: u32,
    pub height: u32,
    /// 動画長 (秒)。これを超えて出現するコメントは描かない。
    pub duration_sec: f64,
    /// フォント倍率。1.0 で niconico 標準相当。
    pub font_scale: f64,
    /// 不透明度 0.0〜1.0。1.0 で完全不透明。
    pub opacity: f64,
    /// 流れるコメントの横断秒数。**本家再現では速度は幅依存で自動決定する**ため
    /// 未使用 (API 互換のため保持)。
    pub scroll_duration_sec: f64,
    /// 固定コメント(および長さ未指定コメント)の既定表示秒数 (本家既定 3 秒)。
    /// `@N` コマンドが無いコメントの `long` として使う。
    pub fixed_duration_sec: f64,
    /// libass に渡すゴシック系フォント名。fontconfig が CJK へフォールバックする。
    pub font_name: String,
}

impl Default for AssOptions {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            duration_sec: 0.0,
            font_scale: 1.0,
            opacity: 1.0,
            scroll_duration_sec: 4.0,
            fixed_duration_sec: 3.0,
            font_name: "sans-serif".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Loc {
    Naka,
    Ue,
    Shita,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Size {
    Small,
    Medium,
    Big,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Font {
    Defont,
    Gothic,
    Mincho,
}

/// コマンド列を解釈した結果 (本家 parseCommandAndNicoScript 相当の必要部分)。
#[derive(Debug, Clone)]
struct ParsedStyle {
    loc: Loc,
    size: Size,
    /// 0xRRGGBB。
    rgb: u32,
    font: Font,
    /// `full` (描画範囲拡張)。
    full: bool,
    /// `ender` (改行リサイズを抑止)。
    ender: bool,
    /// `invisible` (描画スキップ)。
    invisible: bool,
    /// `@N` の長さ (vpos=センチ秒)。未指定は None。
    long: Option<i64>,
    /// nico:opacity / _live による不透明度 (1.0 が既定)。
    opacity: f64,
    /// nico:stroke による縁取り色。0xRRGGBB と alpha(0..1)。
    stroke: Option<(u32, f64)>,
}

// ── 色 ─────────────────────────────────────────────────────────────────

/// niconico の名前付き色 → 0xRRGGBB。無名は None (本家 definition/colors.ts)。
fn named_color(name: &str) -> Option<u32> {
    let c = match name {
        // 通常色
        "white" => 0xFFFFFF,
        "red" => 0xFF0000,
        "pink" => 0xFF8080,
        "orange" => 0xFFC000,
        "yellow" => 0xFFFF00,
        "green" => 0x00FF00,
        "cyan" => 0x00FFFF,
        "blue" => 0x0000FF,
        "purple" => 0xC000FF,
        "black" => 0x000000,
        // プレミアム色 (2 系列・別名含む)。本家は "marinblue" (原典の綴り) だが
        // 一般的な "marineblue" 表記も受ける。
        "white2" | "niconicowhite" => 0xCCCC99,
        "red2" | "truered" => 0xCC0033,
        "pink2" => 0xFF33CC,
        "orange2" | "passionorange" => 0xFF6600,
        "yellow2" | "madyellow" => 0x999900,
        "green2" | "elementalgreen" => 0x00CC66,
        "cyan2" => 0x00CCCC,
        "blue2" | "marinblue" | "marineblue" => 0x3399FF,
        "purple2" | "nobleviolet" => 0x6633CC,
        "black2" | "niconicoblack" => 0x666666,
        _ => return None,
    };
    Some(c)
}

/// `#RRGGBB` / `#RGB` 形式をパース。
fn parse_hex_color(s: &str) -> Option<u32> {
    let hex = s.strip_prefix('#')?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok(),
        3 => {
            // #RGB → #RRGGBB
            let mut chars = hex.chars();
            let r = chars.next()?;
            let g = chars.next()?;
            let b = chars.next()?;
            let expanded: String = [r, r, g, g, b, b].iter().collect();
            u32::from_str_radix(&expanded, 16).ok()
        }
        _ => None,
    }
}

/// 0xRRGGBB → ASS の `&HBBGGRR&` (BGR 順)。
fn ass_color(rgb: u32) -> String {
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8) & 0xFF;
    let b = rgb & 0xFF;
    format!("&H{b:02X}{g:02X}{r:02X}&")
}

/// 不透明度 (0..1) → ASS のアルファバイト 2 桁 (00=不透明, FF=透明)。
fn alpha_hex(opacity: f64) -> String {
    let clamped = opacity.clamp(0.0, 1.0);
    let a = ((1.0 - clamped) * 255.0).round() as u32;
    format!("{a:02X}")
}

/// 本家 getStrokeColor: 既定は黒縁、文字が純黒なら白縁。stroke 指定があれば優先。
/// 戻り値は (0xRRGGBB, alpha 0..1)。
fn stroke_color_for(style: &ParsedStyle) -> (u32, f64) {
    if let Some((rgb, a)) = style.stroke {
        return (rgb, a);
    }
    if style.rgb == 0x000000 {
        (0xFFFFFF, STROKE_OPACITY)
    } else {
        (0x000000, STROKE_OPACITY)
    }
}

// ── コマンド解釈 ──────────────────────────────────────────────────────

fn parse_style(commands: &[String], content: &str, opts: &AssOptions) -> ParsedStyle {
    let mut loc: Option<Loc> = None;
    let mut size: Option<Size> = None;
    let mut rgb: Option<u32> = None;
    let mut font: Option<Font> = None;
    let mut full = false;
    let mut ender = false;
    let mut invisible = false;
    let mut long: Option<i64> = None;
    let mut opacity: f64 = 1.0;
    let mut stroke: Option<(u32, f64)> = None;

    // 本家は「最初に出現したもの勝ち」(??=)。位置・サイズ・色・フォント全て。
    for raw in commands {
        let cmd = raw.trim();
        if cmd.is_empty() {
            continue;
        }
        let lower = cmd.to_ascii_lowercase();

        // nico:stroke:/fill:/opacity: 系
        if let Some(v) = lower.strip_prefix("nico:stroke:") {
            if stroke.is_none() {
                stroke = parse_nico_color_alpha(v);
            }
            continue;
        }
        if let Some(v) = lower.strip_prefix("nico:opacity:") {
            if let Ok(val) = v.parse::<f64>() {
                if val >= 0.0 {
                    opacity = val;
                }
            }
            continue;
        }
        // nico:fill / nico:waku は焼き込みでは未対応 (背景塗り/枠)。無視。
        if lower.starts_with("nico:fill:") || lower.starts_with("nico:waku:") {
            continue;
        }
        // @N (長さ)
        if let Some(rest) = lower.strip_prefix('@') {
            if let Ok(v) = rest.parse::<f64>() {
                long = Some((v * 100.0).floor() as i64);
            }
            continue;
        }

        // 位置・サイズ・フォントは「最初勝ち」(本家 ??= / size は未設定時のみ)。
        match lower.as_str() {
            "ue" => {
                loc.get_or_insert(Loc::Ue);
            }
            "shita" => {
                loc.get_or_insert(Loc::Shita);
            }
            "naka" => {
                loc.get_or_insert(Loc::Naka);
            }
            "big" => {
                size.get_or_insert(Size::Big);
            }
            "small" => {
                size.get_or_insert(Size::Small);
            }
            "medium" => {
                size.get_or_insert(Size::Medium);
            }
            "gothic" => {
                font.get_or_insert(Font::Gothic);
            }
            "mincho" => {
                font.get_or_insert(Font::Mincho);
            }
            "defont" => {
                font.get_or_insert(Font::Defont);
            }
            "full" => full = true,
            "ender" => ender = true,
            "invisible" => invisible = true,
            "_live" => {
                // _live は半透明 (本家 contextFillLiveOpacity = 0.5)。
                if opacity == 1.0 {
                    opacity = 0.5;
                }
            }
            _ => {
                if let Some(c) = named_color(&lower) {
                    rgb.get_or_insert(c);
                } else if let Some(c) = parse_hex_color(cmd) {
                    // プレミアム限定だが、DB にプレミアムフラグが無いので寛容に受ける
                    // (色付き AA・コテハン色を本家同様に再現するため)。
                    rgb.get_or_insert(c);
                }
                // 未知コマンド (184/device/patissier 等) は無視。
            }
        }
    }

    // 本文が "/" で始まる (運営コマンド等) は不可視。
    if content.starts_with('/') {
        invisible = true;
    }

    let loc_final = loc.unwrap_or(Loc::Naka);
    // long 未指定時の既定: naka は本家既定 300cs 固定 (速度を変えないため)。
    // 固定 (ue/shita) はオプションの fixed_duration_sec を既定にできる。
    let long_final = long.or_else(|| {
        if loc_final != Loc::Naka {
            let cs = (opts.fixed_duration_sec * 100.0).round() as i64;
            if cs > 0 {
                return Some(cs);
            }
        }
        None
    });

    ParsedStyle {
        loc: loc_final,
        size: size.unwrap_or(Size::Medium),
        rgb: rgb.unwrap_or(0xFFFFFF),
        font: font.unwrap_or(Font::Defont),
        full,
        ender,
        invisible,
        long: long_final,
        opacity,
        stroke,
    }
}

/// nico:stroke の値 (`RRGGBB` / `RRGGBBAA` / `#...` / 名前) を (rgb, alpha) へ。
fn parse_nico_color_alpha(v: &str) -> Option<(u32, f64)> {
    let v = v.trim();
    if let Some(rgb) = named_color(v) {
        return Some((rgb, STROKE_OPACITY));
    }
    let hex = v.strip_prefix('#').unwrap_or(v);
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => u32::from_str_radix(hex, 16)
            .ok()
            .map(|c| (c, STROKE_OPACITY)),
        3 => parse_hex_color(&format!("#{hex}")).map(|c| (c, STROKE_OPACITY)),
        8 => {
            let rgb = u32::from_str_radix(&hex[0..6], 16).ok()?;
            let a = u32::from_str_radix(&hex[6..8], 16).ok()? as f64 / 255.0;
            Some((rgb, a))
        }
        _ => None,
    }
}

// ── 文字幅計測 (本家は canvas measureText、ここは近似テーブル) ────────────

/// 半角 ASCII (0x20..0x7E) の送り幅 (em=1000 単位)。Helvetica/Arial メトリクスを
/// 基準にした近似。CJK ゴシックの欧文部とも libass のサンセリフとも概ね合う。
const ASCII_ADVANCE: [u16; 95] = [
    278, // ' '
    278, // !
    355, // "
    556, // #
    556, // $
    889, // %
    667, // &
    191, // '
    333, // (
    333, // )
    389, // *
    584, // +
    278, // ,
    333, // -
    278, // .
    278, // /
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556,  // 0-9
    278,  // :
    278,  // ;
    584,  // <
    584,  // =
    584,  // >
    556,  // ?
    1015, // @
    667, 667, 722, 722, 667, 611, 778, 722, 278, 500, // A-J
    667, 556, 833, 722, 778, 667, 778, 722, 667, 611, // K-T
    722, 667, 944, 667, 667, 611, // U-Z
    278, // [
    278, // backslash
    278, // ]
    469, // ^
    556, // _
    333, // `
    556, 556, 500, 556, 556, 278, 556, 556, 222, 222, // a-j
    500, 222, 833, 556, 556, 556, 556, 333, 500, 278, // k-t
    556, 500, 722, 500, 500, 500, // u-z
    334, // {
    260, // |
    334, // }
    584, // ~
];

/// 1 文字の送り幅 (フォント px に対する比率 em)。本家の canvas 計測の近似。
fn char_advance_em(ch: char) -> f64 {
    let code = ch as u32;
    if (0x20..=0x7E).contains(&code) {
        return ASCII_ADVANCE[(code - 0x20) as usize] as f64 / 1000.0;
    }
    // 半角カナ
    if (0xFF61..=0xFF9F).contains(&code) {
        return 0.5;
    }
    // ゼロ幅・結合文字
    if matches!(code, 0x200B..=0x200F | 0xFEFF) || (0x0300..=0x036F).contains(&code) {
        return 0.0;
    }
    if is_fullwidth(code) {
        return 1.0;
    }
    // Latin-1 補助・その他は半角寄りの近似。
    0.55
}

/// 全角 (1em) 扱いするコードポイントか。CJK・かな・ハングル・全角形など。
fn is_fullwidth(code: u32) -> bool {
    matches!(code,
        0x1100..=0x115F |   // Hangul Jamo
        0x2E80..=0x2EFF |   // CJK Radicals
        0x2F00..=0x2FDF |   // Kangxi Radicals
        0x2FF0..=0x2FFF |   // Ideographic Description
        0x3000..=0x303F |   // CJK Symbols & Punctuation (含 U+3000 全角空白)
        0x3040..=0x309F |   // Hiragana
        0x30A0..=0x30FF |   // Katakana
        0x3100..=0x312F |   // Bopomofo
        0x3130..=0x318F |   // Hangul Compatibility Jamo
        0x3190..=0x319F |   // Kanbun
        0x31A0..=0x31BF |   // Bopomofo Extended
        0x31C0..=0x31EF |   // CJK Strokes
        0x31F0..=0x31FF |   // Katakana Phonetic Ext
        0x3200..=0x32FF |   // Enclosed CJK
        0x3300..=0x33FF |   // CJK Compatibility
        0x3400..=0x4DBF |   // CJK Ext A
        0x4E00..=0x9FFF |   // CJK Unified
        0xA000..=0xA4CF |   // Yi
        0xAC00..=0xD7A3 |   // Hangul Syllables
        0xF900..=0xFAFF |   // CJK Compatibility Ideographs
        0xFE30..=0xFE4F |   // CJK Compatibility Forms
        0xFF00..=0xFF60 |   // Fullwidth Forms
        0xFFE0..=0xFFE6 |   // Fullwidth Signs
        0x1F300..=0x1FAFF | // 絵文字
        0x20000..=0x3FFFD | // CJK Ext B+
        0x2003              // EM SPACE (タブ展開先。1em 扱い)
    )
}

// ── フォントメトリクス (本家 niconico.ts を移植) ────────────────────────

/// getCharSize(size): 384 / doubleResized[size] (683 ステージ pre-scale)。
fn char_size_ps(size: Size) -> f64 {
    STAGE_HEIGHT / double_resized(size)
}

/// getLineHeight(size, resized): 683 ステージ pre-scale の行高。
fn line_height_ps(size: Size, resized: bool) -> f64 {
    let base = STAGE_HEIGHT / double_resized(size); // = char_size_ps
    let default_lc = default_line_count(size);
    if resized {
        let rlc = resized_line_count(size);
        (STAGE_HEIGHT - base * (default_lc / rlc)) / (rlc - 1.0)
    } else {
        (STAGE_HEIGHT - base) / (default_lc - 1.0)
    }
}

/// 1 行の font-space 幅 (pre-scale, fontSize = floor(charSize*0.8))。本家
/// measureWidth は各行を ceil する。
fn measure_line_ps(line: &str, char_ps: f64) -> f64 {
    let font_size = (char_ps * 0.8).floor().max(1.0);
    let sum: f64 = line.chars().map(|c| char_advance_em(c) * font_size).sum();
    sum.ceil()
}

// ── コメント幾何 (1920 空間) ─────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Geom {
    index: usize,
    vpos: i64, // センチ秒
    long: i64, // センチ秒
    loc: Loc,
    owner: bool,
    rgb: u32,
    font: Font,
    opacity: f64,
    stroke: (u32, f64),
    lines: Vec<String>,
    /// 1920 空間。
    char_1920: f64,
    line_1920: f64,
    glyph_px: f64,
    width: f64,
    height: f64,
    /// 当たり判定で決まる上端 Y (1920 空間, top 基準。shita は描画時に反転)。
    pos_y: f64,
}

/// 1 コメントの幾何を本家どおり計算する (resizeY/resizeX 込み)。
fn build_geom(
    index: usize,
    vpos: i64,
    style: &ParsedStyle,
    lines: Vec<String>,
    owner: bool,
    font_scale: f64,
) -> Geom {
    let line_count = lines.len().max(1);
    let mut char_ps = char_size_ps(style.size);
    let mut line_ps = line_height_ps(style.size, false);

    // 改行リサイズ (本家 isLineBreakResize): ender でなく行数が閾値以上。
    if !style.ender && line_count >= line_break_count(style.size) {
        let lh_resized = line_height_ps(style.size, true);
        char_ps *= lh_resized / line_ps;
        line_ps = lh_resized;
    }

    // ユーザのフォント倍率を pre-scale で一律に乗せる (当たり判定とも整合)。
    char_ps *= font_scale;
    line_ps *= font_scale;

    // 幅計測 (font-space)。
    let mut width_ps = lines
        .iter()
        .map(|l| measure_line_ps(l, char_ps))
        .fold(0.0_f64, f64::max)
        .max(1.0);

    // 横幅オーバーフローリサイズ (本家 _processResizeX): 固定コメントのみ。
    if style.loc != Loc::Naka {
        let limit = if style.full {
            STAGE_FULL_WIDTH
        } else {
            STAGE_WIDTH
        };
        // 整数フォントの段差で 1 回では収まりきらないことがあるので数回詰める。
        for _ in 0..4 {
            if width_ps <= limit {
                break;
            }
            let scale = limit / width_ps;
            char_ps *= scale;
            line_ps *= scale;
            width_ps = lines
                .iter()
                .map(|l| measure_line_ps(l, char_ps))
                .fold(0.0_f64, f64::max)
                .max(1.0);
        }
    }

    let char_1920 = char_ps * COMMENT_SCALE;
    let line_1920 = line_ps * COMMENT_SCALE;
    let width = width_ps * COMMENT_SCALE;
    let height = line_1920 * (line_count as f64 - 1.0) + char_1920;
    // 字面 px (本家 getFontSizeAndScale: floor(charSize*0.8)、下限 min は scale 吸収)。
    let f08 = char_ps * 0.8;
    let glyph_local = if f08 < MIN_FONT_SIZE {
        f08.max(0.1)
    } else {
        f08.floor()
    };
    let glyph_px = (glyph_local * COMMENT_SCALE).max(1.0);

    Geom {
        index,
        vpos,
        long: style.long.unwrap_or(DEFAULT_LONG_CS).max(1),
        loc: style.loc,
        owner,
        rgb: style.rgb,
        font: style.font,
        opacity: style.opacity,
        stroke: stroke_color_for(style),
        lines,
        char_1920,
        line_1920,
        glyph_px,
        width,
        height,
        pos_y: -1.0,
    }
}

// ── 当たり判定 (本家 utils/comment.ts を移植) ───────────────────────────

/// vpos スロット → そのスロットに居るコメント index 群。
type CollisionMap = HashMap<i64, Vec<usize>>;

/// 決定的擬似乱数 (画面溢れ時の配置用。本家は Math.random だが焼き込みは
/// 再現性が要るので index 由来の LCG で代替する)。
struct Lcg {
    state: u64,
}
impl Lcg {
    fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_mul(2654435761).wrapping_add(1),
        }
    }
    fn next_f64(&mut self) -> f64 {
        // numerical recipes LCG
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.state >> 33) as f64) / ((1u64 << 31) as f64)
    }
}

/// 本家 naka 速度式。
fn naka_speed(width: f64, long: i64) -> f64 {
    (DRAW_RANGE + width * SPEED_OFFSET) / ((long + 100) as f64)
}

/// 本家 getPosX (naka)。vpos はセンチ秒。
fn pos_x(g: &Geom, vpos: f64) -> f64 {
    let speed = naka_speed(g.width, g.long);
    let lapsed = vpos - g.vpos as f64;
    DRAW_PADDING + DRAW_RANGE - (lapsed + 100.0) * speed
}

/// 本家 beforeVpos: コメントが描画開始する相対 vpos。
fn before_vpos(width: f64, long: i64) -> i64 {
    (-288.0 / ((1632.0 + width) / ((long + 125) as f64))).round() as i64 - 100
}

/// 本家 getPosY: 当たり判定から配置できる Y を探す。
/// 戻り値 (新しい currentPos, 変化したか, break すべきか)。
fn get_pos_y(
    current: f64,
    target: &Geom,
    collision: Option<&Vec<usize>>,
    geom: &[Geom],
    rng: &mut Lcg,
) -> (f64, bool, bool) {
    let Some(collision) = collision else {
        return (current, false, false);
    };
    let mut current_pos = current;
    let mut is_changed = false;
    let target_height = target.height;
    // 再帰の代わりに衝突発見で restart。
    'restart: loop {
        for &idx in collision {
            let item = &geom[idx];
            if item.index == target.index || item.pos_y < 0.0 {
                continue;
            }
            // 本家はレイヤー(常に -1)と owner が一致する場合のみ衝突。
            if item.owner == target.owner
                && current_pos < item.pos_y + item.height
                && current_pos + target_height > item.pos_y
            {
                current_pos = item.pos_y + item.height;
                is_changed = true;
                if current_pos + target_height > CANVAS_HEIGHT {
                    if CANVAS_HEIGHT < target_height {
                        if target.loc == Loc::Naka {
                            current_pos = (target_height - CANVAS_HEIGHT) / -2.0;
                        } else {
                            current_pos = 0.0;
                        }
                    } else {
                        current_pos = (rng.next_f64() * (CANVAS_HEIGHT - target_height)).floor();
                    }
                    return (current_pos, true, true);
                }
                continue 'restart;
            }
        }
        break;
    }
    (current_pos, is_changed, false)
}

/// 本家 getMovablePosY: naka コメントの Y を決める。
fn get_movable_pos_y(
    idx: usize,
    geom: &[Geom],
    col_left: &CollisionMap,
    col_right: &CollisionMap,
    rng: &mut Lcg,
) -> f64 {
    let comment = &geom[idx];
    if CANVAS_HEIGHT < comment.height {
        return (comment.height - CANVAS_HEIGHT) / -2.0;
    }
    let width = comment.width;
    let long = comment.long;
    let vpos = comment.vpos;
    let speed = naka_speed(width, long);
    let before = before_vpos(width, long);
    let n = long + 125;

    let mut pos_y = 0.0;
    let mut is_changed = true;
    let mut count = 0;
    let mut last_updated: Option<i64> = None;
    while is_changed && count < 10 {
        is_changed = false;
        count += 1;
        let mut j = before;
        while j < n {
            let v = vpos + j;
            let left = DRAW_PADDING + DRAW_RANGE - ((j + 100) as f64) * speed;
            if last_updated == Some(v) {
                return pos_y;
            }
            let mut is_break = false;
            if left + width >= COLLISION_RIGHT && left <= COLLISION_RIGHT {
                let (p, c, b) = get_pos_y(pos_y, comment, col_right.get(&v), geom, rng);
                pos_y = p;
                is_changed |= c;
                if c {
                    last_updated = Some(v);
                }
                is_break |= b;
            }
            if left + width >= COLLISION_LEFT && left <= COLLISION_LEFT {
                let (p, c, b) = get_pos_y(pos_y, comment, col_left.get(&v), geom, rng);
                pos_y = p;
                is_changed |= c;
                if c {
                    last_updated = Some(v);
                }
                is_break |= b;
            }
            if is_break {
                return pos_y;
            }
            j += 5;
        }
    }
    pos_y
}

/// 本家 processMovableComment の当たり判定登録部 (Y 決定後に呼ぶ)。
fn register_movable(
    idx: usize,
    geom: &[Geom],
    col_left: &mut CollisionMap,
    col_right: &mut CollisionMap,
) {
    let comment = &geom[idx];
    let width = comment.width;
    let long = comment.long;
    let vpos = comment.vpos;
    let speed = naka_speed(width, long);
    let before = before_vpos(width, long);
    let n = long + 125;
    for j in before..n {
        let v = vpos + j;
        let left = DRAW_PADDING + DRAW_RANGE - ((j + 100) as f64) * speed;
        if left + width + COLLISION_PADDING >= COLLISION_RIGHT && left <= COLLISION_RIGHT {
            col_right.entry(v).or_default().push(idx);
        }
        if left + width + COLLISION_PADDING >= COLLISION_LEFT && left <= COLLISION_LEFT {
            col_left.entry(v).or_default().push(idx);
        }
    }
}

/// 本家 getFixedPosY: ue/shita コメントの Y を決める。
fn get_fixed_pos_y(idx: usize, geom: &[Geom], collision: &CollisionMap, rng: &mut Lcg) -> f64 {
    let comment = &geom[idx];
    let long = comment.long;
    let vpos = comment.vpos;
    let mut pos_y = 0.0;
    let mut is_changed = true;
    let mut count = 0;
    while is_changed && count < 10 {
        is_changed = false;
        count += 1;
        for j in 0..long {
            let (p, c, b) = get_pos_y(pos_y, comment, collision.get(&(vpos + j)), geom, rng);
            pos_y = p;
            is_changed |= c;
            if b {
                break;
            }
        }
    }
    pos_y
}

/// 本家 processFixedComment の当たり判定登録部 (Y 決定後に呼ぶ)。
fn register_fixed(idx: usize, geom: &[Geom], collision: &mut CollisionMap) {
    let comment = &geom[idx];
    let long = comment.long;
    let vpos = comment.vpos;
    let collision_end = (long - 20).max(0);
    for j in 0..long {
        if j <= collision_end {
            collision.entry(vpos + j).or_default().push(idx);
        }
    }
}

// ── ASS 文字列生成 ────────────────────────────────────────────────────

/// ASS の Dialogue 本文へ安全に埋め込めるよう 1 行を整形する。
/// `{` `}` `\` をエスケープし、空白は `\h` (ハードスペース) にして libass の
/// 行頭詰め・連続空白詰めを防ぐ (AA の字間を保つ)。
fn ass_escape_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + 8);
    for ch in line.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\r' => {}
            ' ' => out.push_str("\\h"),
            _ => out.push(ch),
        }
    }
    out
}

/// 秒 → ASS タイムコード `H:MM:SS.cs` (センチ秒)。
fn fmt_time(sec: f64) -> String {
    let total_cs = (sec.max(0.0) * 100.0).round() as i64;
    let cs = total_cs % 100;
    let total_s = total_cs / 100;
    let s = total_s % 60;
    let m = (total_s / 60) % 60;
    let h = total_s / 3600;
    format!("{h}:{m:02}:{s:02}.{cs:02}")
}

/// font enum → libass フォント名。
fn font_name_for(font: Font, opts: &AssOptions) -> (String, bool) {
    match font {
        // defont は本家 weight 600 相当なので Bold。
        Font::Defont => (opts.font_name.clone(), true),
        Font::Gothic => (opts.font_name.clone(), false),
        Font::Mincho => ("serif".to_string(), false),
    }
}

/// 1 コメント分の Dialogue 行 (複数行コメントは行ごとに 1 イベント) を push する。
#[allow(clippy::too_many_arguments)]
fn emit_comment(events: &mut String, g: &Geom, opts: &AssOptions, user_opacity: f64) {
    let line_count = g.lines.len().max(1);

    // 不透明度 (ユーザ × コメント)。
    let eff = (user_opacity * g.opacity).clamp(0.0, 1.0);
    let fill_a = alpha_hex(eff);
    let (stroke_rgb, stroke_a) = g.stroke;
    let outline_a = alpha_hex(eff * stroke_a);
    let fill_c = ass_color(g.rgb);
    let stroke_c = ass_color(stroke_rgb);

    // 縁取り太さ: 本家は字面サイズに依らずほぼ一定 (lineWidth 2.8 を drawScale で
    // スケール)。外側はその半分。font_scale ぶんだけ太くする。
    let bord = (CONTEXT_LINE_WIDTH * COMMENT_SCALE / 2.0 * opts.font_scale).max(0.5);

    let (fname, bold) = font_name_for(g.font, opts);
    let b = if bold { 1 } else { 0 };
    let fs = g.glyph_px.round().max(1.0) as i64;

    // 共通タグ。
    let common = format!(
        "\\an4\\fn{fname}\\b{b}\\fs{fs}\\c{fill_c}\\1a&H{fill_a}&\
         \\3c{stroke_c}\\3a&H{outline_a}&\\4a&HFF&\\bord{bord:.1}\\shad0",
    );

    match g.loc {
        Loc::Naka => {
            let speed = naka_speed(g.width, g.long);
            let before = before_vpos(g.width, g.long);
            let start_vpos = (g.vpos + before) as f64;
            // 右端 1920 から左端 -width まで流れ切る vpos。
            let end_vpos = g.vpos as f64 + (1725.0 + g.width) / speed - 100.0;
            let mut start_sec = start_vpos / 100.0;
            let mut end_sec = end_vpos / 100.0;
            if opts.duration_sec > 0.0 {
                end_sec = end_sec.min(opts.duration_sec);
            }
            start_sec = start_sec.max(0.0);
            if end_sec <= start_sec {
                return;
            }
            let x1 = pos_x(g, start_sec * 100.0);
            let x2 = pos_x(g, end_sec * 100.0);
            for (k, line) in g.lines.iter().enumerate() {
                let yc = g.pos_y + g.char_1920 / 2.0 + g.line_1920 * k as f64;
                let _ = line_count;
                events.push_str(&format!(
                    "Dialogue: 0,{st},{en},nnd,,0,0,0,,{{{common}\\move({x1},{y},{x2},{y})}}{txt}\n",
                    st = fmt_time(start_sec),
                    en = fmt_time(end_sec),
                    x1 = x1.round() as i64,
                    x2 = x2.round() as i64,
                    y = yc.round() as i64,
                    txt = ass_escape_line(line),
                ));
            }
        }
        Loc::Ue | Loc::Shita => {
            let start_sec = g.vpos as f64 / 100.0;
            let mut end_sec = (g.vpos + g.long) as f64 / 100.0;
            if opts.duration_sec > 0.0 {
                end_sec = end_sec.min(opts.duration_sec);
            }
            if end_sec <= start_sec.max(0.0) {
                return;
            }
            // 箱の上端。shita は下から積む (本家 draw: canvasHeight - posY - height)。
            let box_top = if g.loc == Loc::Shita {
                CANVAS_HEIGHT - g.pos_y - g.height
            } else {
                g.pos_y
            };
            let x_left = ((CANVAS_WIDTH - g.width) / 2.0).round() as i64;
            for (k, line) in g.lines.iter().enumerate() {
                let yc = box_top + g.char_1920 / 2.0 + g.line_1920 * k as f64;
                events.push_str(&format!(
                    "Dialogue: 0,{st},{en},nnd,,0,0,0,,{{{common}\\pos({x},{y})}}{txt}\n",
                    st = fmt_time(start_sec.max(0.0)),
                    en = fmt_time(end_sec),
                    x = x_left,
                    y = yc.round() as i64,
                    txt = ass_escape_line(line),
                ));
            }
        }
    }
}

/// コメント列から ASS 文字列を生成する。
///
/// 本家 niconicomments の HTML5 描画を 1920×1080 空間で再現し、PlayResX/Y も
/// 1920×1080 に固定する。libass が出力解像度へスケールするので解像度非依存。
pub fn generate_ass(comments: &[BurnInComment], opts: &AssOptions) -> String {
    let font_scale = opts.font_scale.clamp(0.1, 8.0);
    let user_opacity = opts.opacity.clamp(0.0, 1.0);

    // vpos 昇順で処理順を安定させる (当たり判定は処理順依存)。本家は入力順だが、
    // DB は vpos 昇順なので概ね一致する。
    let mut order: Vec<usize> = (0..comments.len()).collect();
    order.sort_by_key(|&i| (comments[i].vpos_ms, i));

    // 幾何を構築 (不可視・空白・動画長超過は除外)。
    let mut geom: Vec<Geom> = Vec::with_capacity(comments.len());
    for &i in &order {
        let c = &comments[i];
        // タブ → 全角空白 2 つ (本家 BaseComment の \t 置換相当)。
        let content = c.content.replace('\t', "\u{2003}\u{2003}");
        let style = parse_style(&c.commands, &content, opts);
        if style.invisible {
            continue;
        }
        if content.trim().is_empty() {
            continue;
        }
        let vpos = c.vpos_ms / 10; // ms → センチ秒
        if opts.duration_sec > 0.0 && (vpos as f64 / 100.0) > opts.duration_sec {
            continue;
        }
        let lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
        let idx = geom.len();
        geom.push(build_geom(idx, vpos, &style, lines, c.is_owner, font_scale));
    }

    // 当たり判定で Y を決める。
    let mut col_left: CollisionMap = HashMap::new();
    let mut col_right: CollisionMap = HashMap::new();
    let mut col_ue: CollisionMap = HashMap::new();
    let mut col_shita: CollisionMap = HashMap::new();
    let mut rng = Lcg::new(0x9E3779B9);

    for idx in 0..geom.len() {
        let pos_y = match geom[idx].loc {
            Loc::Naka => get_movable_pos_y(idx, &geom, &col_left, &col_right, &mut rng),
            Loc::Ue => get_fixed_pos_y(idx, &geom, &col_ue, &mut rng),
            Loc::Shita => get_fixed_pos_y(idx, &geom, &col_shita, &mut rng),
        };
        geom[idx].pos_y = pos_y;
        match geom[idx].loc {
            Loc::Naka => register_movable(idx, &geom, &mut col_left, &mut col_right),
            Loc::Ue => register_fixed(idx, &geom, &mut col_ue),
            Loc::Shita => register_fixed(idx, &geom, &mut col_shita),
        }
    }

    // Dialogue 生成。
    let mut events = String::new();
    for g in &geom {
        emit_comment(&mut events, g, opts, user_opacity);
    }

    // ── ヘッダ ──
    // PlayRes は 1920×1080 固定 (本家内部座標)。libass が動画解像度へスケールする。
    let header = format!(
        "[Script Info]\n\
         ; Generated by Re:NNDD comment burn-in (niconicomments-faithful)\n\
         ScriptType: v4.00+\n\
         PlayResX: 1920\n\
         PlayResY: 1080\n\
         WrapStyle: 2\n\
         ScaledBorderAndShadow: yes\n\
         Collisions: Normal\n\
         YCbCr Matrix: TV.601\n\
         \n\
         [V4+ Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, \
         BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, \
         BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: nnd,{font},75,&H00FFFFFF,&H00FFFFFF,&H66000000,&H00000000,\
         1,0,0,0,100,100,0,0,1,4,0,4,0,0,0,1\n\
         \n\
         [Events]\n\
         Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        font = opts.font_name,
    );

    format!("{header}{events}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn opts() -> AssOptions {
        AssOptions {
            width: 1920,
            height: 1080,
            duration_sec: 600.0,
            ..AssOptions::default()
        }
    }

    fn cmt(vpos_ms: i64, content: &str, commands: &[&str]) -> BurnInComment {
        BurnInComment {
            vpos_ms,
            content: content.to_string(),
            commands: commands.iter().map(|s| s.to_string()).collect(),
            is_owner: false,
        }
    }

    fn dialogues(ass: &str) -> Vec<&str> {
        ass.lines().filter(|l| l.starts_with("Dialogue:")).collect()
    }

    // y 抽出ヘルパ (\move(x1,y,x2,y) の y / \pos(x,y) の y)。
    fn move_y(line: &str) -> i64 {
        let after = line.split("\\move(").nth(1).unwrap();
        after.split(',').nth(1).unwrap().parse().unwrap()
    }
    fn pos_y_of(line: &str) -> i64 {
        let after = line.split("\\pos(").nth(1).unwrap();
        after
            .split(',')
            .nth(1)
            .unwrap()
            .split(')')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }
    fn move_x1(line: &str) -> i64 {
        let after = line.split("\\move(").nth(1).unwrap();
        after.split(',').next().unwrap().parse().unwrap()
    }

    // ---- 色 ----

    #[test]
    fn named_colors_resolve() {
        assert_eq!(named_color("red"), Some(0xFF0000));
        assert_eq!(named_color("white"), Some(0xFFFFFF));
        assert_eq!(named_color("niconicowhite"), Some(0xCCCC99));
        assert_eq!(named_color("truered"), Some(0xCC0033));
        assert_eq!(named_color("marinblue"), Some(0x3399FF));
        assert_eq!(named_color("marineblue"), Some(0x3399FF));
        assert_eq!(named_color("notacolor"), None);
    }

    #[test]
    fn hex_colors_parse() {
        assert_eq!(parse_hex_color("#FF8800"), Some(0xFF8800));
        assert_eq!(parse_hex_color("#f80"), Some(0xFF8800));
        assert_eq!(parse_hex_color("FF8800"), None);
        assert_eq!(parse_hex_color("#xyz"), None);
    }

    #[test]
    fn ass_color_is_bgr() {
        assert_eq!(ass_color(0xFF0000), "&H0000FF&");
        assert_eq!(ass_color(0x00FF00), "&H00FF00&");
        assert_eq!(ass_color(0x0000FF), "&HFF0000&");
    }

    #[test]
    fn alpha_mapping() {
        assert_eq!(alpha_hex(1.0), "00");
        assert_eq!(alpha_hex(0.0), "FF");
        assert_eq!(alpha_hex(0.5), "80");
    }

    #[test]
    fn stroke_default_black_white_for_black_text() {
        let s = parse_style(&["black".into()], "x", &opts());
        assert_eq!(stroke_color_for(&s), (0xFFFFFF, STROKE_OPACITY));
        let s2 = parse_style(&["red".into()], "x", &opts());
        assert_eq!(stroke_color_for(&s2), (0x000000, STROKE_OPACITY));
    }

    // ---- コマンド解釈 (first-wins) ----

    #[test]
    fn parse_defaults() {
        let s = parse_style(&[], "x", &opts());
        assert_eq!(s.loc, Loc::Naka);
        assert_eq!(s.size, Size::Medium);
        assert_eq!(s.rgb, 0xFFFFFF);
        assert_eq!(s.font, Font::Defont);
        assert!(!s.invisible);
    }

    #[test]
    fn parse_position_size_color_font() {
        let s = parse_style(
            &["shita".into(), "big".into(), "red".into(), "gothic".into()],
            "x",
            &opts(),
        );
        assert_eq!(s.loc, Loc::Shita);
        assert_eq!(s.size, Size::Big);
        assert_eq!(s.rgb, 0xFF0000);
        assert_eq!(s.font, Font::Gothic);
    }

    #[test]
    fn parse_first_color_wins() {
        // 本家は ??= で「最初勝ち」。
        let s = parse_style(&["red".into(), "#00FF00".into()], "x", &opts());
        assert_eq!(s.rgb, 0xFF0000);
    }

    #[test]
    fn parse_is_case_insensitive() {
        let s = parse_style(&["UE".into(), "BIG".into(), "Red".into()], "x", &opts());
        assert_eq!(s.loc, Loc::Ue);
        assert_eq!(s.size, Size::Big);
        assert_eq!(s.rgb, 0xFF0000);
    }

    #[test]
    fn parse_invisible_flag_and_slash() {
        assert!(parse_style(&["invisible".into()], "x", &opts()).invisible);
        assert!(parse_style(&[], "/foo", &opts()).invisible);
    }

    #[test]
    fn parse_long_command() {
        let s = parse_style(&["@5".into()], "x", &opts());
        assert_eq!(s.long, Some(500));
    }

    #[test]
    fn parse_premium_hex_accepted() {
        let s = parse_style(&["#123456".into()], "x", &opts());
        assert_eq!(s.rgb, 0x123456);
    }

    // ---- フォントメトリクス ----

    #[test]
    fn char_sizes_match_reference() {
        // 1080 換算 char 箱: small≈65, medium≈95.5, big≈138。
        assert!((char_size_ps(Size::Medium) * COMMENT_SCALE - 95.53).abs() < 0.1);
        assert!((char_size_ps(Size::Big) * COMMENT_SCALE - 138.36).abs() < 0.1);
        assert!((char_size_ps(Size::Small) * COMMENT_SCALE - 65.02).abs() < 0.1);
    }

    #[test]
    fn line_heights_match_reference() {
        assert!((line_height_ps(Size::Medium, false) * COMMENT_SCALE - 81.31).abs() < 0.1);
        assert!((line_height_ps(Size::Big, false) * COMMENT_SCALE - 127.18).abs() < 0.1);
    }

    #[test]
    fn glyph_px_medium_is_about_76() {
        let g = build_geom(
            0,
            0,
            &parse_style(&[], "あ", &opts()),
            vec!["あ".into()],
            false,
            1.0,
        );
        assert!((g.glyph_px - 75.9).abs() < 1.0, "glyph_px={}", g.glyph_px);
        // 単行 medium の箱高 ≈ 95.5。
        assert!((g.height - 95.53).abs() < 0.5, "height={}", g.height);
    }

    #[test]
    fn fullwidth_width_about_one_em() {
        // 全角 5 文字 medium → 幅 ≈ 5*27*COMMENT_SCALE ≈ 379.5。
        let g = build_geom(
            0,
            0,
            &parse_style(&[], "あいうえお", &opts()),
            vec!["あいうえお".into()],
            false,
            1.0,
        );
        assert!((g.width - 379.5).abs() < 3.0, "width={}", g.width);
    }

    #[test]
    fn ascii_narrower_than_fullwidth() {
        let wide = build_geom(
            0,
            0,
            &parse_style(&[], "WWWWW", &opts()),
            vec!["WWWWW".into()],
            false,
            1.0,
        );
        let thin = build_geom(
            0,
            0,
            &parse_style(&[], "iiiii", &opts()),
            vec!["iiiii".into()],
            false,
            1.0,
        );
        assert!(thin.width < wide.width);
        // 半角は全角より狭い。
        let full = build_geom(
            0,
            0,
            &parse_style(&[], "あああああ", &opts()),
            vec!["あああああ".into()],
            false,
            1.0,
        );
        assert!(thin.width < full.width);
    }

    #[test]
    fn line_break_resize_shrinks_tall_comment() {
        // medium 5 行 → resizeY で char が縮む (95.5 → ~49.6)。
        let big = build_geom(
            0,
            0,
            &parse_style(&[], "a", &opts()),
            vec!["a".into()],
            false,
            1.0,
        );
        let many: Vec<String> = (0..5).map(|_| "a".to_string()).collect();
        let resized = build_geom(0, 0, &parse_style(&[], "x", &opts()), many, false, 1.0);
        assert!(
            resized.char_1920 < big.char_1920 * 0.7,
            "resized char {}",
            resized.char_1920
        );
    }

    #[test]
    fn ender_suppresses_line_break_resize() {
        let many: Vec<String> = (0..6).map(|_| "a".to_string()).collect();
        let s = parse_style(&["ender".into()], "x", &opts());
        let g = build_geom(0, 0, &s, many, false, 1.0);
        // ender 付きはリサイズされず medium のまま。
        assert!((g.char_1920 - 95.53).abs() < 0.5);
    }

    #[test]
    fn width_overflow_resizes_fixed_comment() {
        // ue で横に長い → stage 幅 (512*scale≈1439) を超えたら縮む。
        let long_line = "あ".repeat(40); // 40em*27 ≈ 1080 > 512 (pre-scale)
        let s = parse_style(&["ue".into()], &long_line, &opts());
        let g = build_geom(0, 0, &s, vec![long_line.clone()], false, 1.0);
        assert!(
            g.width <= STAGE_WIDTH * COMMENT_SCALE + 2.0,
            "width={}",
            g.width
        );
    }

    #[test]
    fn naka_never_width_resizes() {
        let long_line = "あ".repeat(60);
        let s = parse_style(&[], &long_line, &opts());
        let g = build_geom(0, 0, &s, vec![long_line], false, 1.0);
        // naka は横リサイズしない (画面幅を超えてよい)。
        assert!(g.width > STAGE_WIDTH * COMMENT_SCALE);
    }

    // ---- 速度・位置 ----

    #[test]
    fn naka_speed_is_width_dependent() {
        let narrow = naka_speed(200.0, 300);
        let wide = naka_speed(1000.0, 300);
        assert!(wide > narrow, "wider comment scrolls faster");
    }

    #[test]
    fn pos_x_decreases_over_time() {
        let g = build_geom(
            0,
            0,
            &parse_style(&[], "あ", &opts()),
            vec!["あ".into()],
            false,
            1.0,
        );
        let early = pos_x(&g, 0.0);
        let late = pos_x(&g, 200.0);
        assert!(late < early, "comment moves left over time");
    }

    // ---- 生成全体 ----

    #[test]
    fn header_has_fixed_playres() {
        let ass = generate_ass(&[], &opts());
        assert!(ass.contains("PlayResX: 1920"));
        assert!(ass.contains("PlayResY: 1080"));
        assert!(ass.contains("[V4+ Styles]"));
        assert!(ass.contains("Style: nnd,"));
        assert!(ass.contains("[Events]"));
    }

    #[test]
    fn empty_comments_have_no_dialogue() {
        assert!(dialogues(&generate_ass(&[], &opts())).is_empty());
    }

    #[test]
    fn naka_uses_move_and_an4() {
        let ass = generate_ass(&[cmt(1000, "流れるコメント", &[])], &opts());
        let d = dialogues(&ass);
        assert_eq!(d.len(), 1);
        assert!(d[0].contains("\\move("));
        assert!(d[0].contains("\\an4"));
        assert!(d[0].contains("流れるコメント"));
    }

    #[test]
    fn ue_uses_pos_and_is_top_half() {
        let ass = generate_ass(&[cmt(2000, "上コメ", &["ue"])], &opts());
        let d = dialogues(&ass);
        assert!(d[0].contains("\\pos("));
        assert!(d[0].contains("\\an4"));
        // 上固定 (vpos 2000ms=200cs=2.00s start, long 3s → 5.00s)。
        assert!(d[0].contains("0:00:02.00,0:00:05.00"));
        assert!(pos_y_of(d[0]) < 540);
    }

    #[test]
    fn shita_is_bottom_half() {
        let ass = generate_ass(&[cmt(0, "下コメ", &["shita"])], &opts());
        let d = dialogues(&ass);
        assert!(pos_y_of(d[0]) > 540, "shita y={}", pos_y_of(d[0]));
    }

    #[test]
    fn invisible_and_blank_skipped() {
        assert!(dialogues(&generate_ass(
            &[cmt(1000, "見えない", &["invisible"])],
            &opts()
        ))
        .is_empty());
        assert!(dialogues(&generate_ass(&[cmt(1000, "   ", &[])], &opts())).is_empty());
    }

    #[test]
    fn comment_after_duration_skipped() {
        let mut o = opts();
        o.duration_sec = 5.0;
        assert!(dialogues(&generate_ass(&[cmt(10_000, "遅刻", &[])], &o)).is_empty());
    }

    #[test]
    fn color_command_emits_fill_tag() {
        let ass = generate_ass(&[cmt(0, "赤", &["red"])], &opts());
        assert!(ass.contains("\\c&H0000FF&"));
    }

    #[test]
    fn simultaneous_naka_use_different_lanes() {
        let ass = generate_ass(
            &[cmt(0, "あいうえお", &[]), cmt(0, "かきくけこ", &[])],
            &opts(),
        );
        let d = dialogues(&ass);
        assert_eq!(d.len(), 2);
        assert_ne!(
            move_y(d[0]),
            move_y(d[1]),
            "overlapping comments must not share a lane"
        );
    }

    #[test]
    fn sequential_naka_can_reuse_top_lane() {
        // 1 つめが流れ切ってから 2 つめ → どちらも最上段 (y 同じ)。
        let ass = generate_ass(&[cmt(0, "短", &[]), cmt(20000, "次", &[])], &opts());
        let d = dialogues(&ass);
        assert_eq!(move_y(d[0]), move_y(d[1]));
    }

    #[test]
    fn owner_and_viewer_dont_collide() {
        // 同時刻でも投稿者コメントと視聴者コメントは別レイヤーなので同じ y に乗れる。
        let owner = BurnInComment {
            vpos_ms: 0,
            content: "投稿者".into(),
            commands: vec![],
            is_owner: true,
        };
        let viewer = cmt(0, "視聴者", &[]);
        let ass = generate_ass(&[owner, viewer], &opts());
        let d = dialogues(&ass);
        assert_eq!(d.len(), 2);
        assert_eq!(move_y(d[0]), move_y(d[1]), "owner/viewer share lane 0");
    }

    #[test]
    fn ue_comments_stack_downward() {
        // 同時刻の ue 2 つ → 2 つめは下に積まれる。
        let ass = generate_ass(&[cmt(0, "上1", &["ue"]), cmt(0, "上2", &["ue"])], &opts());
        let d = dialogues(&ass);
        assert_eq!(d.len(), 2);
        let y0 = pos_y_of(d[0]);
        let y1 = pos_y_of(d[1]);
        assert!(y1 > y0, "second ue stacks below first: y0={y0} y1={y1}");
    }

    #[test]
    fn multiline_emits_one_event_per_line() {
        let ass = generate_ass(&[cmt(0, "1行目\n2行目\n3行目", &["ue"])], &opts());
        let d = dialogues(&ass);
        assert_eq!(d.len(), 3);
        // 行ごとに y が下がる。
        assert!(pos_y_of(d[1]) > pos_y_of(d[0]));
        assert!(pos_y_of(d[2]) > pos_y_of(d[1]));
    }

    #[test]
    fn opacity_bakes_into_alpha() {
        let mut o = opts();
        o.opacity = 0.5;
        let ass = generate_ass(&[cmt(0, "半透明", &[])], &o);
        assert!(ass.contains("\\1a&H80&"), "expected 50% fill alpha");
    }

    #[test]
    fn font_name_used_in_header_and_tag() {
        let mut o = opts();
        o.font_name = "Noto Sans CJK JP".into();
        let ass = generate_ass(&[cmt(0, "x", &[])], &o);
        assert!(ass.contains("Style: nnd,Noto Sans CJK JP,"));
        assert!(ass.contains("\\fnNoto Sans CJK JP"));
    }

    #[test]
    fn braces_in_content_escaped() {
        let ass = generate_ass(&[cmt(0, "{evil}", &["ue"])], &opts());
        assert!(ass.contains("\\{evil\\}"));
    }

    #[test]
    fn naka_x_starts_off_right_edge() {
        // 自然な開始 (vpos より前) で左端 x は画面右外 (≳1920)。
        // vpos=0 は t<0 を描けず t=0 へクランプされ既に流れ込んでいるので、
        // クランプされない十分後ろの vpos で検証する。
        let ass = generate_ass(&[cmt(10_000, "あ", &[])], &opts());
        let d = dialogues(&ass);
        let x1 = move_x1(d[0]);
        assert!(x1 > 1900, "start x off right edge, got {x1}");
    }

    #[test]
    fn naka_vpos0_is_clamped_inward() {
        // vpos=0 のコメントは t=0 で既に画面内へ流れ込んでいる (本家同様)。
        let ass = generate_ass(&[cmt(0, "あ", &[])], &opts());
        let d = dialogues(&ass);
        let x1 = move_x1(d[0]);
        assert!(x1 < 1900 && x1 > 1000, "clamped start x, got {x1}");
        assert!(d[0].starts_with("Dialogue: 0,0:00:00.00,"));
    }

    #[test]
    fn time_formatting() {
        assert_eq!(fmt_time(0.0), "0:00:00.00");
        assert_eq!(fmt_time(1.5), "0:00:01.50");
        assert_eq!(fmt_time(61.23), "0:01:01.23");
        assert_eq!(fmt_time(3661.0), "1:01:01.00");
    }

    /// 実 libass で目視検証するためのサンプル ASS を `NNDD_ASS_OUT` (既定
    /// /tmp/nndd_sample.ass) へ書き出す手動ヘルパ。
    /// `cargo test -p nndd-next --lib -- --ignored dump_sample_ass` で実行。
    #[test]
    #[ignore]
    fn dump_sample_ass() {
        let mut o = opts();
        o.duration_sec = 12.0;
        o.font_name = "Noto Sans CJK JP".into();
        let mut cs = vec![
            cmt(200, "本家ニコニコ再現テスト", &[]),
            cmt(200, "wwwwwwwww", &["red"]),
            cmt(400, "あいうえお かきくけこ", &["blue", "big"]),
            cmt(400, "上に固定されるコメント", &["ue"]),
            cmt(600, "下に固定", &["shita", "green"]),
            cmt(600, "small な コメント", &["small"]),
            cmt(800, "黒コメは白縁", &["black"]),
            cmt(1000, "1行目\n2行目\n3行目", &["ue", "yellow"]),
            cmt(
                1000,
                "横にとても長いコメントが画面を流れていく様子の確認用テキスト",
                &[],
            ),
        ];
        for i in 0..15 {
            cs.push(cmt(300 + i * 5, &format!("弾幕{i}"), &[]));
        }
        let ass = generate_ass(&cs, &o);
        let path = std::env::var("NNDD_ASS_OUT").unwrap_or_else(|_| "/tmp/nndd_sample.ass".into());
        std::fs::write(&path, &ass).unwrap();
        eprintln!("wrote {path} ({} bytes)", ass.len());
    }

    #[test]
    fn fixed_duration_option_does_not_change_naka_long() {
        // fixed_duration_sec を変えても naka の long(=速度) は本家既定のまま。
        let mut o = opts();
        o.fixed_duration_sec = 6.0;
        let s_naka = parse_style(&[], "x", &o);
        assert_eq!(s_naka.long, None); // build_geom で 300cs になる
        let s_ue = parse_style(&["ue".into()], "x", &o);
        assert_eq!(s_ue.long, Some(600));
    }

    #[test]
    fn comments_processed_in_vpos_order() {
        // 逆順入力でも当たり判定は vpos 昇順で安定。
        let ass = generate_ass(&[cmt(5000, "後", &[]), cmt(1000, "先", &[])], &opts());
        let d = dialogues(&ass);
        // 先 (vpos 1000) が先に出力される。
        assert!(d[0].contains("先"));
    }
}
