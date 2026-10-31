// Render a page in WebKit at a phone width; print `scrollWidth viewportWidth` of the body
// with every <details> open, and optionally save a full-height PNG (with `js`: run it
// after the tab/mode switch and save one 844 px tall screen, e.g. an open sheet).
// Numbers the js leaves in `window.measure` are printed after the two widths.
// swift webview.swift <page.html> <width> <tab> <mode> [out.png] [js]
import AppKit
import WebKit

let a = CommandLine.arguments
let html = try! String(contentsOfFile: a[1], encoding: .utf8)
let width = CGFloat(Double(a[2])!)
let (tab, mode, png, extra) = (a[3], a[4], a.count > 5 ? a[5] : nil, a.count > 6 ? a[6] : "")
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let web = WKWebView(frame: NSRect(x: 0, y: 0, width: width, height: 844))
let win = NSWindow(contentRect: web.frame, styleMask: [.borderless], backing: .buffered, defer: false)
win.contentView = web

final class Nav: NSObject, WKNavigationDelegate {
    func webView(_ w: WKWebView, didFinish _: WKNavigation!) {
        let js = """
        tab('\(tab)'); mode('\(mode)'); \(extra);
        \(png == nil ? "document.querySelectorAll('details').forEach(d => d.open = true);" : "document.querySelectorAll('section').forEach(s => { const d = s.querySelector('details'); if (d) d.open = true; });")
        [document.body.scrollWidth, document.documentElement.clientWidth, document.documentElement.scrollHeight, ...(window.measure || [])]
        """
        w.evaluateJavaScript(js) { r, e in
            guard let v = r as? [Double] else { print("js error: \(String(describing: e))"); exit(2) }
            print(([Int(v[0]), Int(v[1])].map { "\($0)" } + v.dropFirst(3).map { "\($0)" }).joined(separator: " "))
            guard let png else { exit(0) }
            w.setFrameSize(NSSize(width: width, height: extra.isEmpty ? CGFloat(v[2]) : 844))
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                w.takeSnapshot(with: nil) { img, _ in
                    let rep = NSBitmapImageRep(data: img!.tiffRepresentation!)!
                    try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: png))
                    exit(0)
                }
            }
        }
    }
}
let nav = Nav()
web.navigationDelegate = nav
web.loadHTMLString(html, baseURL: ProcessInfo.processInfo.environment["HERDSMAN_BASE"].flatMap(URL.init(string:)))
DispatchQueue.main.asyncAfter(deadline: .now() + 30) { print("timeout"); exit(3) }
app.run()
