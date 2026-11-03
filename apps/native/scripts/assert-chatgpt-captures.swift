// macOS: assert rendered text after offline waiting/reconnect fixture captures.
// Usage: swift scripts/assert-chatgpt-captures.swift <capture.png>...
import Foundation
import Vision

let paths = Array(CommandLine.arguments.dropFirst())
guard !paths.isEmpty else { fatalError("Provide waiting/reconnect fixture captures.") }
for path in paths {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    try VNImageRequestHandler(url: URL(fileURLWithPath: path)).perform([request])
    let text = (request.results ?? []).compactMap { $0.topCandidates(1).first?.string }
        .joined(separator: " ").lowercased()
    let name = URL(fileURLWithPath: path).lastPathComponent
    guard text.contains("sign-in page is ready to open") else {
        fatalError("Missing rendered setup page text in \(name).")
    }
    let forbidden = #"https?://|id_token_hint|obviously.fake.fixture.hint|/chatgpt/sign.in|auth\.openai\.com"#
    guard text.range(of: forbidden, options: .regularExpression) == nil else {
        fatalError("Rendered setup exposes a URL or token in \(name).")
    }
    print("safe rendered setup: \(name)")
}
