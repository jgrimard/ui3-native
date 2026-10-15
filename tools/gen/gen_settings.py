"""Generates app/src/settings_defs.rs from the UI3 settings defaults array (ui3.js ~lines 940-3995).

Run: python -I tools/gen/gen_settings.py ui3/ui3/ui3.js app/src/settings_defs.rs
"""
import html, re, sys, json

src, dst = sys.argv[1], sys.argv[2]
text = open(src, encoding="utf-8").read()
start = text.index("var defaultSettings =")
end = text.index("];", text.index("ui3_contextMenus_trigger", start))
region = text[start:end]

# Split into top-level objects by brace depth (ignoring braces in strings/comments).
objs, depth, cur, i, n = [], 0, [], 0, len(region)
in_str = None
while i < n:
    c = region[i]
    if in_str:
        cur.append(c)
        if c == "\\":
            cur.append(region[i + 1]); i += 2; continue
        if c == in_str:
            in_str = None
    elif c in "\"'":
        in_str = c; cur.append(c)
    elif region.startswith("//", i):
        j = region.find("\n", i); i = j; continue
    elif c == "{":
        depth += 1
        if depth == 1:
            cur = []
        else:
            cur.append(c)
    elif c == "}":
        depth -= 1
        if depth == 0:
            objs.append("".join(cur))
        else:
            cur.append(c)
    elif depth >= 1:
        cur.append(c)
    i += 1

def parse_js_string(s, i):
    q = s[i]; i += 1; out = []
    while s[i] != q:
        if s[i] == "\\":
            nxt = s[i + 1]
            out.append({"n": "\n", "r": "", "t": "\t"}.get(nxt, nxt)); i += 2
        else:
            out.append(s[i]); i += 1
    return "".join(out), i + 1

def field(body, name):
    m = re.search(r"(?:^|\n)\s*,?\s*" + name + r"\s*:\s*", body)
    if not m:
        return None
    i = m.end()
    c = body[i]
    if c in "\"'":
        v, j = parse_js_string(body, i)
        # string concatenation: "a" + "b"
        rest = body[j:]
        while True:
            mm = re.match(r"\s*\+\s*(['\"])", rest)
            if not mm:
                break
            v2, k = parse_js_string(rest, mm.end() - 1)
            v += v2; rest = rest[k:]
        return ("str", v)
    if c == "[":
        j = body.index("]", i)
        items = []
        k = i + 1
        inner = body[i + 1:j]
        for mm in re.finditer(r"(['\"])((?:\.|(?!\1).)*)\1|(-?\d+(?:\.\d+)?)", inner):
            items.append(mm.group(2) if mm.group(2) is not None else mm.group(3))
        return ("arr", items)
    mm = re.match(r"(-?\d+(?:\.\d+)?)\b", body[i:])
    if mm:
        return ("num", mm.group(1))
    mm = re.match(r"(true|false)\b", body[i:])
    if mm:
        return ("bool", mm.group(1))
    mm = re.match(r"([^\n,]+)", body[i:])
    return ("expr", mm.group(1).strip())

def strip_html(s):
    s = re.sub(r"<br\s*/?>", "\n", s)
    s = re.sub(r"<[^>]+>", "", s)
    return html.unescape(s).strip()

def split_label(s):
    m = re.search(r'<div class="settingDesc">(.*?)</div>', s, re.S)
    desc = strip_html(m.group(1)) if m else ""
    label = strip_html(re.sub(r'<div class="settingDesc">.*?</div>', "", s, flags=re.S))
    return label, desc

CLICK_ALL = ["None", "Maximize/Unmaximize Camera", "Play/Pause", "Fullscreen"]
CLICK_CLIPS = ["None", "Play/Pause", "Fullscreen"]
SPEEDS = ["0.125", "0.25", "0.5", "1", "2", "4", "6", "8", "16", "32", "64", "128", "256", "257"]
# Settings whose default/options are computed by JS expressions in UI3: key -> (default, options or None)
OVERRIDES = {
    "ui3_html5_delay_compensation": ("Normal", ["None", "Weak", "Normal", "Strong"]),
    "ui3_audio_codec": ("μ-law", ["μ-law", "FLAC"]),
    "ui3_singleClick_live": ("Maximize/Unmaximize Camera", CLICK_ALL),
    "ui3_doubleClick_live": ("None", CLICK_ALL),
    "ui3_singleClick_clips": ("Play/Pause", CLICK_CLIPS),
    "ui3_doubleClick_clips": ("Fullscreen", CLICK_CLIPS),
    "ui3_singleClick_timeline": ("Maximize/Unmaximize Camera", CLICK_ALL),
    "ui3_doubleClick_timeline": ("None", CLICK_ALL),
    "ui3_mp4_open_action": ("Always Ask", ["Always Ask", "Open in UI3", "Open in Browser Tab"]),
    "ui3_reverse_speed_substream_threshold": ("2", SPEEDS),
    "ui3_forward_speed_substream_threshold": ("4", SPEEDS),
    "ui3_cameraLabels_text": ("Name", ["Name", "Short Name", "Name (Short Name)"]),
    "ui3_cameraLabels_position": ("Top", ["Above", "Top", "Bottom", "Below"]),
    "ui3_cameraLabels_minimumFontSize": ("8", None),
    # Port decision (patchplan D0.1): default to the direct-to-wire profile so H.265 passes through untouched.
    "ui3_streamingQuality": ("Native (direct H.264/H.265)^", None),
}

FEATURE_TOGGLES = {
    "ui3_feature_enabled_volumeBar": ("Show Volume Bar", "Video Player"),
    "ui3_feature_enabled_profileStatus": ("Show Profile Box", "Side Bar"),
    "ui3_feature_enabled_stopLight": ("Show Traffic Light Button", "Side Bar"),
    "ui3_feature_enabled_globalSchedule": ("Show Schedule Box", "Side Bar"),
    "ui3_feature_enabled_ptzControls": ("Show PTZ Controls", "Side Bar"),
    "ui3_feature_enabled_clipNameLabel": ("Show Clip Name Label", "Clips / Alerts"),
}

# Defaults that are computed at runtime in UI3.
EXPR_DEFAULTS = {
    "ui3_time24hour": "0",
    "ui3_h264_choice4": "Native (D3D11VA)",
    "ui3_mqttInstanceId": "",
}

def rs(s):
    return json.dumps(s, ensure_ascii=False)

out = ["// @generated by tools/gen/gen_settings.py from UI3's defaultSettings array. Do not edit by hand.",
       "use super::settings::{SettingDef, Input, Dv};", "", "pub static DEFS: &[SettingDef] = &["]
count = 0
for body in objs:
    key = field(body, "key")
    if not key:
        continue
    key = key[1]
    val = field(body, "value")
    kind = field(body, "inputType")
    kind = kind[1] if kind else ("hotkey" if field(body, "hotkey") else "")
    if val is None:
        dv = 'Dv::Str("")'
    elif val[0] == "str":
        dv = f"Dv::Str({rs(val[1])})"
    elif val[0] == "num":
        dv = f"Dv::Num({float(val[1])!r})"
    elif val[0] == "bool":
        dv = f"Dv::Str({rs('1' if val[1] == 'true' else '0')})"
    else:
        dv = f"Dv::Str({rs(EXPR_DEFAULTS.get(key, ''))})"
    label_raw = field(body, "label")
    label, desc = split_label(label_raw[1]) if label_raw and label_raw[0] == "str" else ("", "")
    hint = field(body, "hint"); hint = hint[1] if hint and hint[0] == "str" else ""
    cat = field(body, "category"); cat = cat[1] if cat and cat[0] == "str" else ""
    comment = field(body, "comment"); comment = strip_html(comment[1]) if comment and comment[0] == "str" else ""
    opts = field(body, "options"); opts = opts[1] if opts and opts[0] == "arr" else []
    mn = field(body, "minValue"); mx = field(body, "maxValue"); step = field(body, "step"); unit = field(body, "unitLabel")
    num = lambda f, d: float(f[1]) if f and f[0] == "num" else d
    dyn_opts = field(body, "getOptions")
    if key in OVERRIDES:
        d, o = OVERRIDES[key]
        dv = f"Dv::Str({rs(d)})"
        if o is not None:
            opts = o
            dyn_opts = None
    if kind == "select":
        inp = f"Input::Select(&[{', '.join(rs(o) for o in opts)}], {('Some(' + rs(dyn_opts[1]) + ')') if dyn_opts else 'None'})"
    elif kind == "checkbox":
        inp = "Input::Checkbox"
    elif kind in ("number", "range"):
        inp = f"Input::{'Range' if kind == 'range' else 'Number'} {{ min: {num(mn, -1e9)!r}, max: {num(mx, 1e9)!r}, step: {num(step, 1.0)!r}, unit: {rs(unit[1] if unit and unit[0]=='str' else '')} }}"
    elif kind == "color":
        inp = "Input::Color"
    elif kind == "text":
        inp = "Input::Text"
    elif kind == "password":
        inp = "Input::Password"
    elif kind == "comment":
        inp = f"Input::Comment({rs(comment)})"
    elif kind == "hotkey":
        inp = "Input::Hotkey"
    else:
        inp = "Input::Hidden"
    # Port decision (patchplan SET-07): UI3 toggles these with a right-click; the port exposes them as checkboxes.
    if key in FEATURE_TOGGLES:
        inp, label, cat = "Input::Checkbox", FEATURE_TOGGLES[key][0], FEATURE_TOGGLES[key][1]
        hint = "Enable or disable this interface feature."
    out.append(f"    SettingDef {{ key: {rs(key)}, default: {dv}, input: {inp}, label: {rs(label)}, desc: {rs(desc)}, hint: {rs(hint)}, category: {rs(cat)} }},")
    count += 1
out.append("];")
open(dst, "w", encoding="utf-8", newline="\n").write("\n".join(out) + "\n")
print(f"wrote {count} settings to {dst}")
