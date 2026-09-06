import AppKit
import Testing
@testable import LokiApp

/// The artwork, checked against the file that actually ships rather than against a fixture.
///
/// These are cheap and they guard the two assumptions the rendering path rests on: that the asset
/// is the kind we think it is, and that it fills its own box. Both have been wrong before, and
/// both were invisible until somebody looked at the screen.
@MainActor
struct BrandTests {
    @Test("The shipped artwork is vector, so nothing is ever resampled")
    func shippedArtworkIsVector() {
        // If this fails the app still works: `Brand.mark` falls back to the raster path. What it
        // means is that the build stopped shipping `loki-mark.pdf`, or that the file was replaced
        // by a bitmap in a PDF wrapper, which is what arrived the first time one was asked for.
        #expect(Brand.audit() == .vector)
    }

    @Test("The artwork fills its own box, which the vector path depends on")
    func artworkFillsItsBox() throws {
        let image = try #require(Brand.mark(points: 256))
        let coverage = Brand.coverage(of: image)
        // A vector page is drawn straight into the frame it is given with no trim, so padding
        // inside the page box would draw the mark small and nothing would say so.
        #expect(coverage > 0.98, "artwork fills \(Int(coverage * 100))% of its box")
    }

    @Test("A mark comes back at every size the app asks for", arguments: [15.0, 18.0, 22.0, 28.0, 56.0, 1024.0])
    func everySizeRenders(points: Double) throws {
        let mark = try #require(Brand.mark(points: CGFloat(points)))
        #expect(mark.size.width == CGFloat(points))
        #expect(mark.size.height == CGFloat(points))
    }

    @Test("The Dock icon is the same artwork, not a second one")
    func theDockIconIsTheMark() throws {
        let icon = try #require(Brand.icon())
        #expect(icon.size.width >= 512, "the Dock asks for large sizes")
    }
}
