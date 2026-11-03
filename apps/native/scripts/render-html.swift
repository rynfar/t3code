// Hidden WebKit companion capture for --html-fixture; uses the production document-start script.
import AppKit
import WebKit

let args = CommandLine.arguments
let input = try JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: args[1]))) as! [String: Any]
let html = input["html"] as! String
let script = input["script"] as! String
let output = args[2]
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
final class Page: NSObject, WKURLSchemeHandler, WKNavigationDelegate {
    func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
        let valid = task.request.url!.absoluteString == "t3-page://page/index.html"
        let response = HTTPURLResponse(url: task.request.url!, statusCode: valid ? 200 : 404, httpVersion: "HTTP/1.1", headerFields: ["Content-Type":"text/html; charset=utf-8", "Content-Security-Policy":"sandbox allow-scripts allow-forms allow-popups"])!
        task.didReceive(response)
        if valid { task.didReceive(html.data(using: .utf8)!) }
        task.didFinish()
    }
    func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {}
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        let update = input["updateScript"] as? String ?? ""
        webView.evaluateJavaScript("document.querySelector('input')?.setAttribute('data-mounted','yes');" + update) { _, error in
            if let error { fputs("HTML script failed: \(error)\n", stderr); exit(1) }
            webView.evaluateJavaScript("JSON.stringify({background:getComputedStyle(document.documentElement).backgroundColor,font:getComputedStyle(document.documentElement).fontFamily,mono:getComputedStyle(document.querySelector('code')||document.documentElement).fontFamily,mounted:document.querySelector('input')?.getAttribute('data-mounted')})") { value, _ in
                if let json = value as? String { try! json.write(toFile: output + ".json", atomically: true, encoding: .utf8) }
                webView.takeSnapshot(with: nil) { image, error in
                    guard let data = image?.tiffRepresentation, let bitmap = NSBitmapImageRep(data: data), let png = bitmap.representation(using: .png, properties: [:]) else { fputs("HTML capture failed: \(String(describing: error))\n", stderr); exit(1) }
                    try! png.write(to: URL(fileURLWithPath: output))
                    exit(0)
                }
            }
        }
    }
}
let page = Page()
let configuration = WKWebViewConfiguration()
configuration.websiteDataStore = .nonPersistent()
configuration.setURLSchemeHandler(page, forURLScheme: "t3-page")
configuration.userContentController.addUserScript(WKUserScript(source: "window.ipc={postMessage:function(){}};" + script, injectionTime: .atDocumentStart, forMainFrameOnly: true))
let view = WKWebView(frame: NSRect(x: 0, y: 0, width: 728, height: 480), configuration: configuration)
view.appearance = NSAppearance(named: (input["dark"] as? Bool ?? true) ? .darkAqua : .aqua)
view.navigationDelegate = page
view.load(URLRequest(url: URL(string: "t3-page://page/index.html")!))
app.run()
