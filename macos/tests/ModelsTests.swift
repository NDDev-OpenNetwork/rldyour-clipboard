import Foundation
@main struct ModelsTests {
    static func main() throws {
        let data = Data(#"{"id":42,"kind":"text","mimes":["text/html","text/plain;charset=utf-8"],"bytes":10,"preview":"synthetic\ntext","pinned":true,"at":100}"#.utf8)
        let entry = try JSONDecoder().decode(ClipboardEntry.self, from: data)
        precondition(entry.pinned && entry.caption == "synthetic text")
        precondition(ClipboardTypes.supported(entry).count == 2)
        precondition(ClipboardTypes.uti("image/png") == "public.png")
        precondition(ClipboardTypes.uti("unknown") == nil)
        let stats = try JSONDecoder().decode(ClipboardStats.self, from: Data(#"{"entries":1,"pinned":1,"bytes":10,"budget":1000,"retention_days":7}"#.utf8))
        precondition(stats.retention_days == 7)
        print("PASS: typed history, persistent pin metadata and native representation mapping")
    }
}
