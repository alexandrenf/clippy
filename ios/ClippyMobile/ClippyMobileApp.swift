import SwiftUI

@main
struct ClippyMobileApp: App {
    @StateObject private var model: AppModel
    @Environment(\.scenePhase) private var scenePhase

    init() {
        do {
            let configuration = try RuntimeConfiguration()
            _model = StateObject(wrappedValue: AppModel(configuration: configuration))
        } catch {
            fatalError("Clippy sync configuration is incomplete. No secret values should be compiled into the app.")
        }
    }

    var body: some Scene {
        WindowGroup {
            ContentView(model: model, auth: model.auth)
        }
        .onChange(of: scenePhase) { _, phase in
            let activation: UIScene.ActivationState = switch phase {
            case .active: .foregroundActive
            case .inactive: .foregroundInactive
            case .background: .background
            @unknown default: .unattached
            }
            model.sceneChanged(activation)
        }
    }
}
