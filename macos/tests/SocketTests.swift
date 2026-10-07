import Foundation
@main struct SocketTests {
    static func main() {
        let done = DispatchSemaphore(value: 0)
        let client = ClipboardClient(path: CommandLine.arguments[1])
        client.page(pinned: false, query: "native socket synthetic", before: nil) { result in
            let page = try! result.get()
            precondition(page.stats.retention_days == 7)
            precondition(page.items.count == 2)
            let group = DispatchGroup()
            for entry in page.items {
                group.enter()
                client.pin(entry) { result in
                    _ = try! result.get()
                    client.fetch(entry) { result in
                        let transfers = try! result.get()
                        precondition(transfers.count == 1)
                        let data = transfers[0].data
                        if data.count > 1024 {
                            let prefix = Data("native socket synthetic large:".utf8)
                            precondition(data.count == prefix.count + 3 * 1024 * 1024)
                            precondition(data.starts(with: prefix))
                            precondition(data.dropFirst(prefix.count).allSatisfy { $0 == 0x78 })
                        } else {
                            precondition(String(data: data, encoding: .utf8) == "native socket synthetic")
                        }
                        group.leave()
                    }
                }
            }
            group.notify(queue: .global()) { done.signal() }
        }
        precondition(done.wait(timeout: .now() + 15) == .success)
        print("PASS: native RPC history, retention metadata, durable pins and byte-exact small/3MiB mapped restore")
    }
}
