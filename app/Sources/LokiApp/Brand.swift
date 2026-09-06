import SwiftUI
import os

/// **Everything about the mark, in one place.**
///
/// **`branding/logo/` is the source, and the only thing to edit.** `scripts/build-app.sh` copies
/// `logo.png` to `Resources/loki-mark.png` on every build and generates the Dock icon from the
/// same file, so the mark in the thread, the mark in the title bar and the icon in the Dock are
/// all one piece of artwork. The copy is generated: change it there, not here.
///
/// It is a copy rather than a symlink because SwiftPM will not follow one into a resource bundle.
/// The build refreshing it every time is what stops it drifting.
///
/// | File | Used for |
/// |---|---|
/// | `logo.png` | The app and Dock icon, installed by `scripts/build-app.sh` |
/// | `logo-glowing.png` | The glowing variant. Not used in the app; the glow is drawn |
/// | `logo-black-bg.png` | Flattened onto black, for anywhere transparency is unwelcome |
/// | `logo-text-only.png` | The wordmark. For a banner or a README, not for the interface |
/// | `logo.ico`, `logo-glowing.ico`, `logo-text-only.ico` | Windows and favicon formats |
enum Brand {
    /// Where the artwork lives, relative to the repository root.
    static let artworkDirectory = "branding/logo"
    /// The file the app icon is generated from.
    static let iconFile = "logo.png"

    /// The mark, trimmed and sized, cached per point size.
    ///
    /// Three things were wrong with handing the raw file to SwiftUI, and they compounded.
    ///
    /// **The file is 2400px but reports 900pt**, because the DPI baked into it is 192 rather than
    /// 72. AppKit therefore believed it was a 900 point image, which is why the menu bar tried to
    /// lay out a logo the height of the screen.
    ///
    /// **Twelve percent of every side is transparent padding.** The circle fills 76% of the frame,
    /// so a 22pt avatar was drawing a 16pt mark floating in a box, which reads as small and badly
    /// aligned rather than as deliberate.
    ///
    /// **A resizable image inside `scaleEffect` rasterises at its layout size and then scales the
    /// bitmap**, which is what made it look pixelated. Rendering at the size actually wanted, once,
    /// fixes it at the source.
    static func mark(points: CGFloat) -> NSImage? {
        // **Vector goes straight through, and that is the whole point of it.** Everything below
        // this line exists to make an unavoidable resample as good as it can be: trim the padding,
        // one representation per device scale, never scale after the fact. Core Graphics
        // rasterises a PDF at the size *and the subpixel position* it is actually drawn at, so
        // there is no resample to improve. No cache either: caching would put a bitmap back in the
        // path and take the position back out of it.
        if let vector = vector(points: points) { return vector }

        let key = Int(points.rounded())
        if let cached = cache.withLock({ $0[key] }) { return cached }
        guard let trimmed else { return nil }

        let image = NSImage(size: NSSize(width: points, height: points))
        for scale in Self.scales {
            image.addRepresentation(rasterise(trimmed, points: points, scale: scale))
        }
        cache.withLock { $0[key] = image }
        return image
    }

    /// The device scales a Mac actually asks for.
    ///
    /// **One representation per scale, and every one of them an exact integer.** An earlier version
    /// carried a single 3x representation on the theory that more pixels is safer. It is not: a 2x
    /// display then needs 44 pixels from a 66 pixel bitmap, and resampling by two thirds at draw
    /// time is what put stair-steps on a circle that is clean in the source file. With a
    /// representation per scale AppKit picks the matching one and never resamples, on either
    /// display, including when a window is dragged from one to the other.
    private static let scales: [CGFloat] = [1, 2, 3]

    /// One representation, drawn at exactly `points * scale` pixels.
    private static func rasterise(
        _ source: NSImage,
        points: CGFloat,
        scale: CGFloat
    ) -> NSBitmapImageRep {
        let pixels = Int((points * scale).rounded())
        let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil,
            pixelsWide: pixels,
            pixelsHigh: pixels,
            bitsPerSample: 8,
            samplesPerPixel: 4,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: .deviceRGB,
            bytesPerRow: 0,
            bitsPerPixel: 0
        )!
        rep.size = NSSize(width: points, height: points)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        NSGraphicsContext.current?.imageInterpolation = .high
        source.draw(
            in: NSRect(x: 0, y: 0, width: points, height: points),
            from: .zero,
            operation: .sourceOver,
            fraction: 1
        )
        NSGraphicsContext.restoreGraphicsState()
        return rep
    }

    /// Puts the mark on the Dock tile, and does it again whenever the tile is rebuilt.
    ///
    /// **Changing the activation policy destroys the tile and takes the icon with it.** The window
    /// controller flips to `.regular` every time the thread opens and to `.accessory` when it
    /// closes, so an icon applied once at launch survived until the first window and no longer.
    /// An assembled `.app` reads `CFBundleIconFile` and never noticed; the bare executable Xcode
    /// runs has no such file and showed the generic one (B-71).
    static func applyToDock() {
        guard let icon = icon() else { return }
        NSApplication.shared.applicationIconImage = icon
        // The tile does not repaint itself when the image behind it changes.
        NSApp.dockTile.display()
    }

    /// The mark at full resolution, for the Dock.
    static func icon() -> NSImage? {
        // Large, because the Dock asks for sizes up to 1024 and a vector page has no natural one
        // to give it.
        vector(points: 1024) ?? trimmed
    }

    private static let cache = OSAllocatedUnfairLock(initialState: [Int: NSImage]())

    /// The artwork with its transparent margin cropped away, so the circle fills whatever box it
    /// is given.
    private static let trimmed: NSImage? = {
        guard let source = loaded, let cg = pixels(of: source) else { return nil }
        let box = opaqueBounds(of: cg)
        guard let cropped = cg.cropping(to: box) else { return nil }
        return NSImage(cgImage: cropped, size: NSSize(width: box.width, height: box.height))
    }()

    /// The tightest rectangle containing every pixel that is not fully transparent.
    private static func opaqueBounds(of image: CGImage) -> CGRect {
        let w = image.width, h = image.height
        guard let context = CGContext(
            data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return CGRect(x: 0, y: 0, width: w, height: h) }
        context.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        guard let data = context.data else { return CGRect(x: 0, y: 0, width: w, height: h) }
        let pixels = data.bindMemory(to: UInt8.self, capacity: w * h * 4)

        var minX = w, minY = h, maxX = 0, maxY = 0
        for y in 0..<h {
            for x in 0..<w where pixels[(y * w + x) * 4 + 3] > 16 {
                if x < minX { minX = x }
                if x > maxX { maxX = x }
                if y < minY { minY = y }
                if y > maxY { maxY = y }
            }
        }
        guard minX <= maxX, minY <= maxY else { return CGRect(x: 0, y: 0, width: w, height: h) }
        return CGRect(x: minX, y: minY, width: maxX - minX + 1, height: maxY - minY + 1)
    }

    /// The artwork, straight off disk. **A PDF if one is there, the PNG otherwise.**
    ///
    /// **Vector is the only thing that removes the last class of defect here rather than reducing
    /// it.** A bitmap has to be resampled to reach a size, and every bitmap fix in this file is a
    /// way of making that resample as good as it can be: trim once, one representation per device
    /// scale, never scale after the fact. All of it still fails on a fractional position, because
    /// a bitmap sampled half a pixel off is a blurred bitmap, and a centred layout on an
    /// odd-width window produces exactly that. Measured: a half-pixel offset costs 70 mean channel
    /// error at 1x, which is more than every other defect in this file put together.
    ///
    /// Core Graphics rasterises a PDF analytically at the exact size *and the exact position* it
    /// is asked for, so the resample stops existing. Drop `logo.pdf` into `branding/logo/` and it
    /// is picked up with no other change; until then the PNG path is what runs.
    private static let loaded: NSImage? = {
        for name in ["loki-mark", "logo"] {
            for ext in ["pdf", "png"] {
                if let found = load(name, ext) { return found }
            }
        }
        return nil
    }()

    /// **By URL, not by name.** `Image("name", bundle:)` is an asset-catalog lookup, and a loose
    /// file is not an asset, so it resolves to nothing and draws nothing: no crash, no warning,
    /// just a hole where the logo should be.
    private static func load(_ name: String, _ ext: String) -> NSImage? {
        for bundle in [Bundle.module, Bundle.main] {
            if let url = bundle.url(forResource: name, withExtension: ext),
               let image = NSImage(contentsOf: url) {
                return image
            }
        }
        if let nested = Bundle.main.url(forResource: "LokiApp_LokiApp", withExtension: "bundle"),
           let bundle = Bundle(url: nested),
           let url = bundle.url(forResource: name, withExtension: ext),
           let image = NSImage(contentsOf: url) {
            return image
        }
        return nil
    }

    /// The artwork sized for drawing, if it is vector.
    ///
    /// Returns `nil` for a raster asset, and for a PDF that is really a bitmap in a wrapper: that
    /// file draws no better as a page than as pixels, and its page box carries padding the raster
    /// path would trim and this one would not.
    private static func vector(points: CGFloat) -> NSImage? {
        guard let loaded, isVector, let sized = loaded.copy() as? NSImage else { return nil }
        sized.size = NSSize(width: points, height: points)
        return sized
    }

    /// Whether the artwork is genuinely vector rather than a bitmap in a PDF wrapper.
    ///
    /// A wrapped bitmap presents as an `NSPDFImageRep` like any other PDF, so the container is not
    /// the question. What separates them is whether the page draws from pixels: a real vector page
    /// has no image in it at all.
    static var isVector: Bool {
        guard let page = loaded?.representations.first(where: { $0 is NSPDFImageRep })
                as? NSPDFImageRep
        else { return false }
        return !containsBitmap(page.pdfRepresentation)
    }

    /// Whether a PDF's bytes declare an image XObject.
    ///
    /// A byte scan rather than a parse. It is looking for one token in an uncompressed object
    /// dictionary, which is where a page's resources are declared even when its content stream is
    /// compressed, and being wrong in the safe direction costs a fallback to the raster path.
    private static func containsBitmap(_ pdf: Data) -> Bool {
        for marker in ["/Subtype /Image", "/Subtype/Image"] {
            if let needle = marker.data(using: .ascii), pdf.range(of: needle) != nil {
                return true
            }
        }
        return false
    }

    /// The artwork as pixels, whatever container it arrived in.
    ///
    /// **A PDF is passed through the same trim and the same per-scale rendering as a PNG, rather
    /// than handed to the layout as a resizable page.** The tempting shortcut is to set a PDF's
    /// size and let Core Graphics rasterise it analytically, which is genuinely better for a true
    /// vector file. It is wrong for the file we have: `logo.pdf` is a 3752px bitmap in a PDF
    /// wrapper, with 42 path operators that only place it, and its page box carries padding. Passed
    /// through it would have drawn the mark smaller than its box, which is the bug the trim exists
    /// to prevent, reintroduced by the fix for a different one.
    ///
    /// Rasterising at `sourcePixels` and trimming covers both cases: a real vector file gets a
    /// sampling source far larger than any size drawn here, and a wrapped bitmap gets its own
    /// pixels. Nothing has to know which it was given.
    private static let sourcePixels = 2048

    private static func pixels(of image: NSImage) -> CGImage? {
        guard image.representations.contains(where: { $0 is NSPDFImageRep }) else {
            return image.cgImage(forProposedRect: nil, context: nil, hints: nil)
        }
        let side = CGFloat(sourcePixels)
        guard let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil,
            pixelsWide: sourcePixels,
            pixelsHigh: sourcePixels,
            bitsPerSample: 8,
            samplesPerPixel: 4,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: .deviceRGB,
            bytesPerRow: 0,
            bitsPerPixel: 0
        ) else { return nil }
        rep.size = NSSize(width: side, height: side)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        NSGraphicsContext.current?.imageInterpolation = .high
        image.draw(
            in: NSRect(x: 0, y: 0, width: side, height: side),
            from: .zero,
            operation: .sourceOver,
            fraction: 1
        )
        NSGraphicsContext.restoreGraphicsState()
        return rep.cgImage
    }
}

#if DEBUG
extension Brand {
    /// What the shipped artwork actually is, for the checks below and for a preview to report.
    enum Kind: Equatable {
        case vector
        case raster(fills: CGFloat)
        case missing
    }

    /// Rasterises the shipped asset and measures how much of its own box it occupies.
    ///
    /// **The assumption the vector path rests on.** A vector page is drawn straight into the frame
    /// it is given, with no trim, so artwork inset inside its own page box would draw small. The
    /// PNG path trims and does not care; this one does. Dropping in a padded asset would be a
    /// silent regression of exactly the defect the trim was written for, so it is measured rather
    /// than assumed.
    static func audit() -> Kind {
        guard let loaded else { return .missing }
        guard isVector else {
            return .raster(fills: coverage(of: loaded))
        }
        return .vector
    }

    static func coverage(of image: NSImage) -> CGFloat {
        guard let cg = pixels(of: image) else { return 0 }
        let box = opaqueBounds(of: cg)
        return box.width / CGFloat(cg.width)
    }
}
#endif
