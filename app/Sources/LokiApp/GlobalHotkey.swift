import AppKit
import Carbon.HIToolbox

/// A system-wide hotkey.
///
/// Carbon's `RegisterEventHotKey` rather than `NSEvent.addGlobalMonitorForEvents`. The monitor
/// needs accessibility permission, which cannot be granted programmatically and would put a
/// system settings trip in the way of first launch. The Carbon call needs nothing: verified
/// registering successfully with `AXIsProcessTrusted()` false.
///
/// The handler runs on the main thread, because Carbon dispatches on the run loop that installed
/// it.
@MainActor
final class GlobalHotkey {
    /// Option and Space. Reaches Loki from any app.
    static let optionSpace = GlobalHotkey(keyCode: UInt32(kVK_Space), modifiers: UInt32(optionKey))

    private let keyCode: UInt32
    private let modifiers: UInt32
    private var hotKey: EventHotKeyRef?
    private var handler: EventHandlerRef?

    private init(keyCode: UInt32, modifiers: UInt32) {
        self.keyCode = keyCode
        self.modifiers = modifiers
    }

    var isRegistered: Bool { hotKey != nil }

    /// Claims the hotkey. Returns false if another app already holds it.
    ///
    /// A clash is a normal outcome, not an error: the user has some other tool bound to the same
    /// keys. The app carries on and the menu bar still works.
    @discardableResult
    func register(_ action: @escaping @MainActor () -> Void) -> Bool {
        guard hotKey == nil else { return true }
        Self.action = action

        var spec = EventTypeSpec(
            eventClass: OSType(kEventClassKeyboard),
            eventKind: UInt32(kEventHotKeyPressed)
        )
        InstallEventHandler(GetEventDispatcherTarget(), Self.callback, 1, &spec, nil, &handler)

        let id = EventHotKeyID(signature: Self.signature, id: 1)
        let status = RegisterEventHotKey(
            keyCode,
            modifiers,
            id,
            GetEventDispatcherTarget(),
            0,
            &hotKey
        )

        if status != noErr {
            unregister()
            return false
        }
        return true
    }

    func unregister() {
        if let hotKey {
            UnregisterEventHotKey(hotKey)
            self.hotKey = nil
        }
        if let handler {
            RemoveEventHandler(handler)
            self.handler = nil
        }
        Self.action = nil
    }

    /// 'LOKI'.
    private static let signature = OSType(0x4C4F_4B49)

    /// Reached from a C callback, which cannot capture context.
    ///
    /// Safety invariant: written only from `register` and `unregister`, both `@MainActor`, and
    /// read only from the Carbon handler, which Carbon dispatches on the same main run loop.
    /// One thread throughout.
    private nonisolated(unsafe) static var action: (@MainActor () -> Void)?

    private static let callback: EventHandlerUPP = { _, event, _ in
        var id = EventHotKeyID()
        let status = GetEventParameter(
            event,
            EventParamName(kEventParamDirectObject),
            EventParamType(typeEventHotKeyID),
            nil,
            MemoryLayout<EventHotKeyID>.size,
            nil,
            &id
        )
        guard status == noErr, id.signature == GlobalHotkey.signature else {
            return OSStatus(eventNotHandledErr)
        }
        MainActor.assumeIsolated { GlobalHotkey.action?() }
        return noErr
    }
}
