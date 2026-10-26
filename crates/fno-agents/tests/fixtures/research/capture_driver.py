"""Capture driver for the research family goldens (tests/golden/research/pure.json).

Imports the worktree's Python leg (`fno.research`) and freezes every pure
decision the Rust leg replays: the query floor, the slug, the ddgs output
parse, the SSRF verdict for literal addresses, the HTML-to-text extraction,
the content-type and charset reads, the source row shape, the store reader,
the brief render and the output_dir config read. Nothing here touches the
network or DNS: the guard cases use literal IPs or a patched resolver.

Run from the repo root with an interpreter that has the cli deps:

    PYTHONPATH=cli/src uv run --project cli python \
        crates/fno-agents/tests/fixtures/research/capture_driver.py
"""

import email.message
import importlib
import json
import os
import random
import socket
import subprocess
import sys
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path

FIXTURE_DIR = Path(__file__).resolve().parent
TESTS_DIR = FIXTURE_DIR.parents[1]
GOLDEN = TESTS_DIR / "golden" / "research" / "pure.json"

from fno.research import core  # noqa: E402
from fno.research import deliverable as deli  # noqa: E402

CASES = []


def add(fn, inp, out):
    CASES.append({"fn": fn, "input": inp, "output": out})


def capture_normalize():
    topics = [
        "",
        "tesla",
        "   ",
        "two words",
        "  CA   CCLD\tfinancials  ",
        "a\nb",
        "one two",
        "it's thin",
        'say "hi"',
        "both ' and \"",
        "tab\there",
        "x\x01y",
        "café",
        "é",
        "line sep word",
        "back\\slash",
        "\x0b\x0cvt ff\r\n",
        "　wide　space　",
        "a\x1cb",
        "­",
        "emoji \U0001f600",
        "solo\x7f",
    ]
    for t in topics:
        try:
            add("normalize_query", {"topic": t}, {"ok": core.normalize_query(t)})
        except core.EmptyQuery as e:
            add("normalize_query", {"topic": t}, {"err": str(e)})


def capture_slugify():
    for t in [
        "",
        "CA CCLD financials",
        "---",
        "Hello, World!",
        "  leading and trailing  ",
        "café crème",
        "K kelvin",
        "İstanbul",
        "a" * 70,
        ("ab " * 30),
        ("x" * 59) + "-y",
        ("x" * 59) + " yz",
        "UPPER lower 123",
        "ééé",
        "snake_case_words",
        "dots.and/slashes",
        "ΣΣ final",
    ]:
        add("slugify", {"topic": t}, core.slugify(t))


def capture_parse_ddgs():
    raws = [
        "",
        "   ",
        "not json",
        "[]",
        "{}",
        "null",
        "42",
        '"str"',
        '[{"href": "https://a.example/x"}]',
        '[{"href": "https://a.example/x"}, {"href": "https://a.example/x"}]',
        '[{"url": "http://b.example"}, {"link": "https://c.example"}]',
        '[{"href": "", "url": "https://fallback.example"}]',
        '[{"href": null, "link": "https://l.example"}]',
        '[{"href": "ftp://x.example"}, {"href": "HTTPS://upper.example"}]',
        '[{"href": 5}, "row", 7, null, {"title": "no url"}]',
        '{"results": [{"href": "https://r.example"}]}',
        '{"results": [], "data": [{"href": "https://d.example"}]}',
        '{"results": "oops"}',
        '{"data": {"href": "https://notalist.example"}}',
        '{"other": [{"href": "https://o.example"}]}',
        '  [{"href": "https://ws.example"}]  \n',
        '[{"href": "https://u.example/café"}]',
        '[{"href": "https://x.example", "url": "https://y.example"}]',
        '[{"href": false, "url": 0, "link": "http://z.example"}]',
        '[{"href": "javascript:alert(1)"}]',
    ]
    for raw in raws:
        add("parse_ddgs", {"raw": raw}, core._parse_ddgs(raw))


def capture_strip_html():
    bodies = [
        "",
        "plain text",
        "<html><script>bad()</script><body><h1>Hi</h1> there</body></html>",
        "x < y and y > z",
        "a<b>bold</b>c",
        "<p>one</p><p>two</p>",
        "<style>.a{color:red}</style>visible",
        "<SCRIPT>if (a < b) { x = '</div>'; }</SCRIPT>after",
        "<script>never closed",
        "before<script>never closed",
        "text &amp; more &lt;tag&gt; &#65; &#x42; &copy; &notanentity;",
        "trailing &amp",
        "tom &jerry",
        "tom &jerry and friends",
        "a &; b",
        "&#0; &#128; &#xD800; &#1114112; &#11; &#x1F600;",
        "&amp;amp; &ampx &AMP; &Aacute &Aacutex",
        "&#99999999999999999999;",
        "<!-- comment -->kept",
        "<!--unterminated comment",
        "<!-->abrupt",
        "<!--->abrupt2",
        "<!DOCTYPE html><title>T &amp; t</title>body",
        "<?xml version='1.0'?>pi",
        "<![CDATA[x<y]]>after cdata",
        "<!bogus>after",
        "</>empty end",
        "</ notatag>after",
        "<br/>a<br />b<img src='x'>c",
        "<a href=\"x\">link</a>",
        "<a b='x>broken",
        "<div class='c' data-x=\"y\" hidden>z</div>",
        "<textarea>&lt;raw&gt; <b>no tag</b></textarea>after",
        "<xmp><b>shown raw</b> &amp;</xmp>tail",
        "<iframe>inner</iframe>out",
        "<noscript><b>n</b></noscript>",
        "  lots   of\t\tspaces \r\n\n\n\n\nand blank lines  ",
        "line1\n\n\nline2",
        "\x0b\x0cvt",
        "<p>café — naïve</p>",
        "<",
        "a <",
        "a < ",
        "<1notatag>",
        "<a",
        "text <a",
        "<script/>visible",
        "<style/>also",
        "<b>unclosed bold",
        "<p\nclass='x'>newline attr</p>",
        "<a / b>slash attr</a>",
        "<a href= 'q>x",
        "<a href = \"v\">sp</a>",
        "<a/>self",
        "<a x=1/>z",
        "</script>stray end",
        "<script>a</script >b",
        "<script>a</scriptx>b</script>c",
        "<title>A &amp; B</title>",
        "&nbsp;nbsp&nbsp;",
        " nbsp-raw ",
        "<p>&quot;quoted&quot; &apos;</p>",
        "<p>end with amp &</p>",
        "no tags but & alone",
        "x&#",
        "x&#;",
        "x&#x;",
        "<a\tb='1'>tab</a>",
        "<a b=\"1\"c=\"2\">adj</a>",
        "<a =x>eq start</a>",
        "<!DOCTYPE never closed",
        "<?pi never closed",
        "<![if x]>cond<![endif]>",
        "<p>x</p >y</ p>z",
    ]
    # A seeded spray of tokenizer fragments: the constructs the hand-picked
    # cases cover, recombined so their interactions are frozen too.
    pieces = list("<>/!?-=\"' \t\n&;#xab1") + [
        "script", "style", "<a ", "</a>", "<!--", "-->", "&amp;", "&#x41;", "<![CDATA[",
        "]]>", "<title>", "</title>", "<xmp>", "é", "&lt", "</script>", "<script>",
        "<!DOCTYPE", "<p>", "</p>", " href='", "=", "&not",
    ]
    rnd = random.Random(11)
    for _ in range(80):
        bodies.append("".join(rnd.choice(pieces) for _ in range(rnd.randint(1, 30))))
    for b in bodies:
        add("strip_html", {"body": b}, core._strip_html(b))


def capture_content_type():
    cts = [
        "",
        "text/html",
        "text/html; charset=utf-8",
        "TEXT/PLAIN",
        "application/json",
        "application/ld+json",
        "application/xml",
        "application/xhtml+xml",
        "application/pdf",
        "image/png",
        "application/octet-stream",
        "xtext/html",
        "my-text/html",
        "application/jsonp",
        "Application/JSON",
        "multipart/form-data; boundary=text/",
        " text/css",
    ]
    for ct in cts:
        add("text_content_type", {"ct": ct}, bool(core._TEXT_CT_RE.search(ct or "")))
    for ct in [
        "",
        "text/html",
        "text/html; charset=utf-8",
        "text/html; charset=UTF-8",
        'text/html; charset="ISO-8859-1"',
        "text/html;charset=windows-1252",
        "text/html; Charset=Shift_JIS",
        "text/html; charset=",
        "text/html; foo=bar; charset=us-ascii",
        "text/html; charset = utf-8",
        "text/plain; charset='utf-8'",
    ]:
        m = email.message.Message()
        if ct:
            m["Content-Type"] = ct
        add("content_charset", {"ct": ct}, m.get_content_charset())


def capture_decode():
    samples = [
        ("utf-8", "café \U0001f600".encode("utf-8")),
        ("utf-8", b"bad \xff\xfe bytes \xe2\x82"),
        ("utf8", b"plain"),
        ("iso-8859-1", b"caf\xe9"),
        ("latin-1", b"\x80\xff"),
        ("us-ascii", b"ascii \xe9"),
        ("ascii", b"ok"),
        ("windows-1252", b"\x93quoted\x94 \x80 \x81"),
        ("cp1252", b"\x9f\x8d"),
    ]
    for cs, raw in samples:
        add("decode_body", {"charset": cs, "hex": raw.hex()}, raw.decode(cs, errors="replace"))


def capture_hash():
    for t in ["", "body text", "café", "\U0001f600" * 3]:
        add("content_hash", {"text": t}, core.content_hash(t))


def capture_isoformat():
    for us in [0, 1, 999999, 1_700_000_000_000_000, 1_700_000_000_123_456, 1_790_000_000_000_001]:
        dt = datetime(1970, 1, 1, tzinfo=timezone.utc) + timedelta(microseconds=us)
        add("isoformat", {"epoch_us": us}, dt.isoformat())


def _dt(us):
    return datetime(1970, 1, 1, tzinfo=timezone.utc) + timedelta(microseconds=us)


def capture_build_source():
    now_us = 1_790_000_000_123_456
    fetches = [
        ("https://x", core.FetchResult(True, "body text", "text/html", 200, "")),
        ("https://y", core.FetchResult(False, "", "", 404, "http 404")),
        ("https://z", core.FetchResult(True, "q" * 4100, "text/plain", 200, "")),
        ("https://u", core.FetchResult(True, "café \"quote\" \\ \n\t\x01  ", "text/html", 200, "")),
        ("https://e", core.FetchResult(True, "", "text/html", 200, "")),
        ("https://b", core.FetchResult(False, "", "", None, "blocked non-public host: x -> 127.0.0.1")),
        ("https://w", core.FetchResult(True, "\U0001f600" * 4001, "text/html", 200, "")),
    ]
    for url, f in fetches:
        src = core.build_source(url, f, now=_dt(now_us))
        add(
            "build_source",
            {
                "url": url,
                "ok": f.ok,
                "text": f.text,
                "reason": f.reason,
                "epoch_us": now_us,
            },
            src.to_json_line(),
        )


def capture_read_sources():
    texts = [
        "",
        "\n\n",
        core.Source("https://a", "2026-01-01", "h", "x", True).to_json_line()
        + "\n{not json}\n"
        + '["a bare list is not a row"]\n'
        + "42\n"
        + core.Source("https://b", "2026-01-01", "", "", False, "http 500").to_json_line()
        + "\n",
        '{"url": "https://only-url"}\n',
        '{"fetched_at": "t"}\n',
        '{"url": 5, "verified": 1, "fetched_at": null, "extract": "e"}\n',
        '{"url": "https://v", "verified": "yes"}\n{"url": "https://w", "verified": ""}\n',
        '{"url": "https://v", "verified": [], "extra": {"k": [1, 2.5, true]}}\n',
        "  " + core.Source("https://pad", "t", "h", "e", True).to_json_line() + "  \r\n",
        core.Source("https://ls", "t", "h", "split here", True).to_json_line() + "\n",
        core.Source("https://nel", "t", "h", "x\x85y", True).to_json_line() + "\n",
        '{"url":"https://compact","verified":true,"hash":"h","extract":"e","fetched_at":"t","reason":""}\n',
        '{"url": "https://f", "fetched_at": 1.5, "hash": 1e20, "extract": -0.0, "reason": 3}\n',
        '{"url": "https://dup", "url": "https://dup2"}\n',
        '{"url": "https://uni", "extract": "\\u00e9\\ud83d\\ude00"}\n',
    ]
    for text in texts:
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "t.sources.jsonl"
            p.write_text(text, encoding="utf-8")
            rows = core.read_sources(p)
        add(
            "read_sources",
            {"text": text},
            [{"line": r.to_json_line(), "verified": r.verified} for r in rows],
        )


def _addrinfo(ip):
    if ":" in ip:
        return [(socket.AF_INET6, 1, 6, "", (ip, 0, 0, 0))]
    return [(socket.AF_INET, 1, 6, "", (ip, 0))]


def capture_guard():
    ips = [
        "127.0.0.1",
        "127.255.0.9",
        "169.254.169.254",
        "10.0.0.9",
        "172.16.0.1",
        "172.31.255.255",
        "172.32.0.1",
        "192.168.1.1",
        "192.0.0.9",
        "192.0.0.10",
        "192.0.0.8",
        "192.0.0.170",
        "192.0.2.5",
        "198.18.0.1",
        "198.51.100.7",
        "203.0.113.9",
        "100.64.0.1",
        "0.0.0.0",
        "0.1.2.3",
        "224.0.0.1",
        "239.255.255.250",
        "240.0.0.1",
        "255.255.255.255",
        "8.8.8.8",
        "1.1.1.1",
        "93.184.216.34",
        "::1",
        "::",
        "::ffff:127.0.0.1",
        "::ffff:8.8.8.8",
        "::ffff:10.1.2.3",
        "fe80::1",
        "fc00::1",
        "fd12:3456::1",
        "ff02::1",
        "2001:db8::1",
        "2001:4860:4860::8888",
        "2606:4700:4700::1111",
        "2002::1",
        "64:ff9b:1::1",
        "64:ff9b::808:808",
        "100::1",
        "2001:1::1",
        "2001:3::1",
        "2001:20::1",
        "fec0::1",
        "3fff::1",
        "4000::1",
        "fe00::1",
    ]
    real = socket.getaddrinfo
    for ip in ips:
        socket.getaddrinfo = lambda *a, _ip=ip, **k: _addrinfo(_ip)
        try:
            core._guard_url("https://evil.example/meta")
            out = "ok"
        except core.BlockedHost as e:
            out = str(e.reason)
        finally:
            socket.getaddrinfo = real
        add("guard_resolved", {"host": "evil.example", "ips": [ip]}, out)
    socket.getaddrinfo = lambda *a, **k: _addrinfo("8.8.8.8") + _addrinfo("10.0.0.1")
    try:
        core._guard_url("https://mixed.example/")
        out = "ok"
    except core.BlockedHost as e:
        out = str(e.reason)
    finally:
        socket.getaddrinfo = real
    add("guard_resolved", {"host": "mixed.example", "ips": ["8.8.8.8", "10.0.0.1"]}, out)

    # Literal-address and scheme cases resolve without DNS.
    urls = [
        "ftp://example.com/x",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "example.com/no-scheme",
        "http://",
        "https:///path-only",
        "http://127.0.0.1/",
        "http://127.0.0.1:8080/admin",
        "HTTP://10.0.0.1/",
        "https://[::1]/",
        "https://[::ffff:127.0.0.1]:443/x",
        "http://8.8.8.8/",
        "https://[2606:4700:4700::1111]/",
        "http://user:pw@192.168.0.1/",
        "http://169.254.169.254/latest/meta-data/",
        "gopher://1.1.1.1/",
        "http://0.0.0.0/",
    ]
    for url in urls:
        try:
            core._guard_url(url)
            out = "ok"
        except core.BlockedHost as e:
            out = str(e.reason)
        add("guard_url", {"url": url}, out)


def _vsrc(url, extract, verified=True):
    return core.Source(url=url, fetched_at="2026-06-23T00:00:00+00:00", hash="abc" if verified else "",
                       extract=extract, verified=verified, reason="" if verified else "http 404")


def capture_brief():
    now = _dt(1_790_000_000_000_000)
    sets = [
        ("my research topic", "declared", [
            _vsrc("https://a.example/x", "alpha finding text"),
            _vsrc("https://b.example/y", "beta finding text"),
        ]),
        ("topic two words", "declared", [
            _vsrc("https://a.example/x", "alpha"),
            _vsrc("https://dead.example", "", verified=False),
        ]),
        ("nothing found topic", "declared", []),
        ("only dead sources", "cap 3", [_vsrc("https://dead.example", "", verified=False)]),
        ("long snippet topic", "cap 2", [
            _vsrc("https://l.example", "word " * 100),
            _vsrc("https://e.example", ""),
            _vsrc("https://w.example", "  spaced\n\tout text  "),
        ]),
        ('quote "in" topic', "declared", [_vsrc("https://q.example", "café — x")]),
    ]
    for topic, stopped, srcs in sets:
        slug = core.slugify(topic)
        add(
            "build_brief",
            {
                "topic": topic,
                "slug": slug,
                "stopped": stopped,
                "epoch_us": 1_790_000_000_000_000,
                "sources": [json.loads(s.to_json_line()) for s in srcs],
            },
            deli.build_brief(topic, slug, srcs, stopped=stopped, now=now),
        )
    for text in ["", "a  b\n c", "x" * 300, " " * 5, ("ab " * 100), "　wide　", "y" * 239 + " z"]:
        add("snippet", {"text": text}, deli._snippet(text))


def capture_output_dir():
    for configured in [None, "", "   ", "\t\n"]:
        try:
            deli.resolve_output_dir(configured)
            out = {"ok": True}
        except deli.OutputDirUnset as e:
            out = {"err": str(e)}
        add("output_dir_unset", {"configured": configured}, out)

    docs = [
        "schema_version: 1\n",
        "schema_version: 1\nconfig:\n  research:\n    output_dir: ~/c3po/raw/readyrule\n",
        "schema_version: 1\nresearch:\n  output_dir: /flat/x\n",
        'schema_version: 1\nconfig:\n  research:\n    output_dir: "/a/*"\n',
        'schema_version: 1\nconfig:\n  research:\n    output_dir: "/a/[b]"\n',
        'schema_version: 1\nconfig:\n  research:\n    output_dir: "  /pad  "\n',
        "schema_version: 1\nconfig:\n  research:\n    output_dir: 5\n",
        "schema_version: 1\nconfig:\n  research:\n    output_dir: null\n",
        "schema_version: 1\nconfig:\n  research:\n    output_dir: ''\n",
        "schema_version: 1\nconfig:\n  research: {}\n",
        "schema_version: 1\nconfig:\n  research:\n    output_dir: " + "p" * 4100 + "\n",
    ]
    saved = {k: os.environ.get(k) for k in ("HOME", "FNO_GLOBAL_SETTINGS_PATH", "FNO_CONFIG")}
    cwd = os.getcwd()
    try:
        for text in docs:
            with tempfile.TemporaryDirectory() as d:
                t = Path(d)
                (t / "p" / ".fno").mkdir(parents=True)
                f = t / "p" / ".fno" / "settings.yaml"
                f.write_text(text, encoding="utf-8")
                os.environ["HOME"] = str(t)
                os.environ["FNO_GLOBAL_SETTINGS_PATH"] = str(t / "none.yaml")
                os.environ["FNO_CONFIG"] = str(f)
                os.chdir(t)
                import fno.config as cfg

                importlib.reload(cfg)
                try:
                    out = cfg.load_settings().research.output_dir
                except Exception:  # noqa: BLE001 - the CLI reads any refusal as unset
                    out = None
                os.chdir(cwd)
            add("output_dir_from_yaml", {"text": text}, out)
    finally:
        os.chdir(cwd)
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


def capture_timeout_message():
    for q, m in [("two words", 10), ("it's quoted", 5), ("x y", -1)]:
        cmd = ["ddgs", "text", "-q", q, "-m", str(m), "-o", "json"]
        e = subprocess.TimeoutExpired(cmd, 30)
        add("ddgs_timeout_message", {"query": q, "max_results": m},
            str(core.DdgsUnavailable(f"ddgs search timed out: {e}")))


def main():
    capture_normalize()
    capture_slugify()
    capture_parse_ddgs()
    capture_strip_html()
    capture_content_type()
    capture_decode()
    capture_hash()
    capture_isoformat()
    capture_build_source()
    capture_read_sources()
    capture_guard()
    capture_brief()
    capture_output_dir()
    capture_timeout_message()
    GOLDEN.parent.mkdir(parents=True, exist_ok=True)
    doc = {"oracle": "fno.research (Python leg, captured before deletion)", "cases": CASES}
    GOLDEN.write_text(json.dumps(doc, indent=1, ensure_ascii=True) + "\n", encoding="utf-8")
    print(f"wrote {len(CASES)} cases to {GOLDEN}", file=sys.stderr)


if __name__ == "__main__":
    main()
