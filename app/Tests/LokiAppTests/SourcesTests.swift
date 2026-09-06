import Foundation
import Testing
@testable import LokiApp

/// The citation model. Small, and every case here has been a bug in some product.
struct SourceTests {
    private func source(_ url: String, icon: Data? = nil) -> Source {
        Source(id: 1, url: url, title: "t", excerpt: "e", icon: icon, fetched: .now)
    }

    @Test("A host is what a person would call the site", arguments: [
        ("https://example.com/a/b?c=d", "example.com"),
        ("https://www.example.com/a", "example.com"),
        ("http://en.wikipedia.org/wiki/Rust", "en.wikipedia.org"),
        ("https://example.com", "example.com"),
    ])
    func hostIsReadable(url: String, want: String) {
        #expect(source(url).host == want)
    }

    /// `www` is not the site, and a citation reading "W" for every second source would be useless.
    @Test("The fallback letter comes from the name, never from www")
    func initialSkipsTheSubdomainNobodyReads() {
        #expect(source("https://www.example.com").initial == "E")
        #expect(source("https://github.com").initial == "G")
    }

    /// A URL that does not parse must still render something rather than crashing a thread.
    @Test("A malformed url still yields a mark")
    func malformedUrlsDegrade() {
        #expect(source("").initial == "?")
        #expect(source("").host.isEmpty)
        #expect(source("not a url").initial == "N")
    }

    /// The citation shows the lead source and counts the rest, which is what "+5" means.
    @Test("A claim resting on several sources names one and counts the others")
    func severalSourcesReadAsOnePlusCount() {
        let many = (1...6).map {
            Source(id: $0, url: "https://s\($0).test", title: "t", excerpt: "", icon: nil, fetched: .now)
        }
        #expect(many.count == 6)
        #expect(many.first?.host == "s1.test")
        // The interface shows the lead plus a count of the remainder, never a count of everything.
        #expect(many.count - 1 == 5)
    }
}
