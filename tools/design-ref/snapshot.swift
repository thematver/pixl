// Renders the Claude Design export (docs/design/source/PIXL.dc.html) into reference PNGs,
// one per screen state, using the system WebKit. Used only as a visual reference for the
// native GUI (golden comparisons); not part of the shipped plugin.
//
//   swift tools/design-ref/snapshot.swift <source_dir> <out_dir>
import AppKit
import WebKit

let args = CommandLine.arguments
let srcDir = URL(fileURLWithPath: args.count > 1 ? args[1] : "docs/design/source")
let outDir = URL(fileURLWithPath: args.count > 2 ? args[2] : "docs/design/ref")

// id, channel, focus, overlay, chip
let screens: [(String, String, String, String, String)] = [
    ("01-wave", "wave", "none", "none", "GB"),
    ("02-pulse1", "p1", "none", "none", "GB"),
    ("03-noise", "noise", "none", "none", "GB"),
    ("04-nes-triangle", "wave", "none", "none", "NES"),
    ("05-sweep", "wave", "sweep", "none", "GB"),
    ("06-vibrato", "wave", "vibrato", "none", "GB"),
    ("07-crush", "wave", "crush", "none", "GB"),
    ("08-glide", "p1", "glide", "none", "GB"),
    ("09-unison", "p2", "unison", "none", "GB"),
    ("10-browser", "wave", "none", "browser", "GB"),
    ("11-macro-assign", "wave", "none", "macro", "GB"),
]

let template = try! String(contentsOf: srcDir.appendingPathComponent("PIXL.dc.html"), encoding: .utf8)
try? FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)

final class Snapper: NSObject, WKNavigationDelegate {
    var queue = screens
    let web: WKWebView
    let window: NSWindow
    override init() {
        let cfg = WKWebViewConfiguration()
        web = WKWebView(frame: NSRect(x: 0, y: 0, width: 900, height: 560), configuration: cfg)
        window = NSWindow(contentRect: NSRect(x: -3000, y: -3000, width: 900, height: 560), styleMask: [.borderless], backing: .buffered, defer: false)
        super.init()
        window.contentView = web
        window.orderBack(nil)
        web.navigationDelegate = self
    }
    func next() {
        guard let s = queue.first else { NSApp.terminate(nil); return }
        var html = template
        func setDefault(_ key: String, _ val: String) {
            // data-props is HTML-entity encoded JSON: &quot;key&quot;:{...&quot;default&quot;:&quot;X&quot;
            let pattern = "(&quot;\(key)&quot;:\\{[^}]*?&quot;default&quot;:&quot;)[^&]*(&quot;)"
            let re = try! NSRegularExpression(pattern: pattern)
            html = re.stringByReplacingMatches(in: html, range: NSRange(html.startIndex..., in: html), withTemplate: "$1\(val)$2")
        }
        setDefault("channel", s.1); setDefault("focus", s.2); setDefault("overlay", s.3); setDefault("chip", s.4)
        // Neutral page background around the 900×560 card.
        html = html.replacingOccurrences(of: "body{margin:0;background:#E7E4DE", with: "body{margin:0;background:#E7E4DE;overflow:hidden")
        let tmp = srcDir.appendingPathComponent(".snap-\(s.0).dc.html")
        try! html.write(to: tmp, atomically: true, encoding: .utf8)
        web.loadFileURL(tmp, allowingReadAccessTo: srcDir)
    }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        // Wait for React/Babel + web fonts to settle.
        DispatchQueue.main.asyncAfter(deadline: .now() + 4.0) {
            let cfg = WKSnapshotConfiguration()
            cfg.rect = NSRect(x: 0, y: 0, width: 900, height: 560)
            cfg.snapshotWidth = 1800
            webView.takeSnapshot(with: cfg) { image, err in
                let s = self.queue.removeFirst()
                try? FileManager.default.removeItem(at: srcDir.appendingPathComponent(".snap-\(s.0).dc.html"))
                if let image, let tiff = image.tiffRepresentation, let rep = NSBitmapImageRep(data: tiff),
                   let png = rep.representation(using: .png, properties: [:]) {
                    try! png.write(to: outDir.appendingPathComponent("\(s.0).png"))
                    print("wrote \(s.0).png")
                } else {
                    print("FAILED \(s.0): \(String(describing: err))")
                }
                self.next()
            }
        }
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let snapper = Snapper()
DispatchQueue.main.async { snapper.next() }
app.run()
