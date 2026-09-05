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

    /// The mark at full resolution, for the Dock.
    ///
    /// **Set at launch, not left to the bundle.** `CFBundleIconFile` only applies to an assembled
    /// `.app`, and the app is routinely run as the bare SwiftPM executable from Xcode, where there
    /// is no `Resources/` for an `.icns` to sit in and the Dock falls back to the generic
    /// executable icon. Assigning it at runtime covers both, and costs one line.
    static func icon() -> NSImage? { trimmed }

    private static let cache = OSAllocatedUnfairLock(initialState: [Int: NSImage]())

    /// The artwork with its transparent margin cropped away, so the circle fills whatever box it
    /// is given.
    private static let trimmed: NSImage? = {
        guard let source = loaded,
              let cg = source.cgImage(forProposedRect: nil, context: nil, hints: nil)
        else { return nil }
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

    /// The file, straight off disk.
    ///
    /// **By URL, not by name.** `Image("loki-mark", bundle:)` does an asset-catalog lookup, and a
    /// loose PNG is not an asset, so it resolves to nothing and draws nothing: no crash, no
    /// warning, just a hole where the logo should be.
    private static let loaded: NSImage? = {
        let named = "loki-mark"
        for bundle in [Bundle.module, Bundle.main] {
            if let url = bundle.url(forResource: named, withExtension: "png"),
               let image = NSImage(contentsOf: url) {
                return image
            }
        }
        if let nested = Bundle.main.url(forResource: "LokiApp_LokiApp", withExtension: "bundle"),
           let bundle = Bundle(url: nested),
           let url = bundle.url(forResource: named, withExtension: "png"),
           let image = NSImage(contentsOf: url) {
            return image
        }
        return nil
    }()
}
