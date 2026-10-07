import Foundation
@main struct SocketTests {
    static func main() {
        let done = DispatchSemaphore(value: 0)
        let client = ClipboardClient(path: CommandLine.arguments[1])
        client.page(pinned: false, query: "native socket synthetic", before: nil) { result in
            let page = try! result.get()
            precondition(page.stats.retention_days == 7)
            precondition(!page.items.isEmpty)
            let entry = page.items[0]
            client.pin(entry) { result in
                _ = try! result.get()
                client.fetch(entry) { result in
                    let transfers = try! result.get()
                    precondition(transfers.count == 1)
                    let content = try! String(contentsOf: transfers[0].url, encoding: .utf8)
                    precondition(content == "native socket synthetic")
                    done.signal()
                }
            }
        }
        precondition(done.wait(timeout: .now() + 10) == .success)
        print("PASS: native RPC history, retention metadata, durable pin and byte-exact restore")
    }
}
