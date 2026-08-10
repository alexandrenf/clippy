import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    @ObservedObject var model: AppModel
    @ObservedObject var auth: AuthController

    @State private var newSectionName = ""
    @State private var itemDrafts: [UUID: String] = [:]
    @State private var editingItem: LocalItem?
    @State private var editingText = ""
    @State private var renamingSection: LocalSection?
    @State private var renamedSectionName = ""
    @State private var importingForItem: UUID?
    @State private var showsFileImporter = false

    var body: some View {
        NavigationStack {
            ZStack {
                ClippyPalette.canvas.ignoresSafeArea()

                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        appHeader
                        statusBanner

                        if !auth.signedIn {
                            signedOutCard
                        } else if model.library.actorId.isEmpty {
                            accountConnectionCard
                        } else {
                            librarySections
                        }

                        if let message = model.message {
                            messageCard(message)
                        }

                        privacyFooter
                    }
                    .padding(.horizontal, 20)
                    .padding(.top, 14)
                    .padding(.bottom, 30)
                }
                .scrollIndicators(.hidden)
            }
            .toolbar(.hidden, for: .navigationBar)
        }
        .tint(ClippyPalette.accent)
        .preferredColorScheme(.light)
        .sheet(item: $editingItem) { item in
            itemEditor(item)
        }
        .alert("Rename list", isPresented: Binding(
            get: { renamingSection != nil },
            set: { if !$0 { renamingSection = nil } }
        )) {
            TextField("List name", text: $renamedSectionName)
            Button("Cancel", role: .cancel) { renamingSection = nil }
            Button("Save") {
                if let section = renamingSection {
                    model.renameSection(id: section.id, name: renamedSectionName)
                }
                renamingSection = nil
            }
        }
        .fileImporter(
            isPresented: $showsFileImporter,
            allowedContentTypes: [.data],
            allowsMultipleSelection: false
        ) { result in
            defer { importingForItem = nil }
            guard let itemId = importingForItem,
                  case let .success(urls) = result,
                  let url = urls.first else { return }
            model.addAttachment(itemId: itemId, url: url)
        }
    }

    private var appHeader: some View {
        HStack(spacing: 13) {
            ZStack {
                RoundedRectangle(cornerRadius: 15, style: .continuous)
                    .fill(ClippyPalette.accentPastel)
                Image(systemName: "paperclip")
                    .font(.system(size: 22, weight: .semibold))
                    .foregroundStyle(ClippyPalette.accent)
                    .rotationEffect(.degrees(-8))
            }
            .frame(width: 48, height: 48)
            .overlay {
                RoundedRectangle(cornerRadius: 15, style: .continuous)
                    .stroke(ClippyPalette.accent.opacity(0.14), lineWidth: 1)
            }

            VStack(alignment: .leading, spacing: 1) {
                Text("Clippy")
                    .font(.system(size: 30, weight: .bold, design: .rounded))
                    .tracking(-0.8)
                    .foregroundStyle(ClippyPalette.text)
                Text("Your lists, wherever you are")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(ClippyPalette.muted)
            }

            Spacer(minLength: 8)

            if !model.library.actorId.isEmpty {
                Button {
                    model.syncNow()
                } label: {
                    Image(systemName: "arrow.triangle.2.circlepath")
                        .font(.system(size: 16, weight: .semibold))
                        .frame(width: 38, height: 38)
                        .background(ClippyPalette.surface, in: Circle())
                        .overlay { Circle().stroke(ClippyPalette.hairline, lineWidth: 1) }
                }
                .buttonStyle(ClippyIconButtonStyle())
                .disabled(model.syncState == .syncing)
                .accessibilityLabel("Sync now")
            } else if auth.signedIn {
                Menu {
                    Button("Sign out", systemImage: "rectangle.portrait.and.arrow.right") {
                        auth.signOut()
                    }
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.system(size: 17, weight: .semibold))
                        .frame(width: 38, height: 38)
                        .background(ClippyPalette.surface, in: Circle())
                        .overlay { Circle().stroke(ClippyPalette.hairline, lineWidth: 1) }
                }
                .accessibilityLabel("Account options")
            }
        }
    }

    private var statusBanner: some View {
        HStack(spacing: 12) {
            ZStack {
                Circle().fill(statusTint)
                Image(systemName: statusSymbol)
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(statusAccent)
            }
            .frame(width: 38, height: 38)

            VStack(alignment: .leading, spacing: 2) {
                Text(statusText)
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(ClippyPalette.text)
                Text(statusDetail)
                    .font(.system(size: 12.5, weight: .medium))
                    .foregroundStyle(ClippyPalette.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Spacer(minLength: 8)

            if model.syncState == .syncing {
                ProgressView()
                    .controlSize(.small)
                    .tint(statusAccent)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 13)
        .background(statusBackground, in: RoundedRectangle(cornerRadius: 19, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 19, style: .continuous)
                .stroke(statusAccent.opacity(0.12), lineWidth: 1)
        }
        .accessibilityElement(children: .combine)
    }

    private var signedOutCard: some View {
        card {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Connect your Clippy")
                        .font(.system(size: 22, weight: .bold, design: .rounded))
                        .tracking(-0.35)
                        .foregroundStyle(ClippyPalette.text)
                    Text("Sign in with the same account as your Mac. Your lists and files connect automatically—no pairing code needed.")
                        .font(.system(size: 15, weight: .regular))
                        .foregroundStyle(ClippyPalette.muted)
                        .lineSpacing(3)
                        .fixedSize(horizontal: false, vertical: true)
                }

                Button {
                    auth.signIn()
                } label: {
                    HStack(spacing: 9) {
                        Image(systemName: "envelope.fill")
                        Text("Continue with email")
                        Spacer()
                        Image(systemName: "arrow.right")
                    }
                }
                .buttonStyle(ClippyPrimaryButtonStyle())

                if let error = auth.errorMessage {
                    Label(error, systemImage: "exclamationmark.circle.fill")
                        .font(.system(size: 12.5, weight: .medium))
                        .foregroundStyle(ClippyPalette.danger)
                        .fixedSize(horizontal: false, vertical: true)
                }

                HStack(spacing: 7) {
                    Image(systemName: "lock.fill")
                    Text("Magic link sign-in · no password stored")
                }
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(ClippyPalette.muted)
            }
        }
    }

    private var accountConnectionCard: some View {
        card {
            VStack(alignment: .leading, spacing: 17) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Connecting your account")
                        .font(.system(size: 22, weight: .bold, design: .rounded))
                        .tracking(-0.35)
                        .foregroundStyle(ClippyPalette.text)
                    Text("Clippy is securely finding your Mac and bringing over your lists. Keep Clippy open on the Mac for a moment.")
                        .font(.system(size: 15))
                        .foregroundStyle(ClippyPalette.muted)
                        .lineSpacing(3)
                        .fixedSize(horizontal: false, vertical: true)
                }

                HStack(spacing: 11) {
                    ProgressView()
                        .controlSize(.small)
                        .tint(ClippyPalette.accent)
                    Text(model.connectingAccount ? "Looking for your Mac…" : "Waiting for your Mac…")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(ClippyPalette.muted)
                }
            }
        }
    }

    private var librarySections: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .firstTextBaseline) {
                Text("Your lists")
                    .font(.system(size: 22, weight: .bold, design: .rounded))
                    .tracking(-0.3)
                    .foregroundStyle(ClippyPalette.text)
                Spacer()
                Text("\(model.library.sections.count)")
                    .font(.system(size: 13, weight: .semibold, design: .rounded))
                    .foregroundStyle(ClippyPalette.accent)
                    .padding(.horizontal, 9)
                    .padding(.vertical, 4)
                    .background(ClippyPalette.accentPastel, in: Capsule())
            }

            HStack(spacing: 10) {
                TextField("New list name", text: $newSectionName)
                    .textInputAutocapitalization(.sentences)
                    .submitLabel(.done)
                    .onSubmit(createSection)
                    .padding(.horizontal, 13)
                    .padding(.vertical, 11)
                    .background(ClippyPalette.surface, in: RoundedRectangle(cornerRadius: 13, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: 13, style: .continuous)
                            .stroke(ClippyPalette.hairline, lineWidth: 1)
                    }

                Button(action: createSection) {
                    Image(systemName: "plus")
                        .font(.system(size: 16, weight: .bold))
                        .frame(width: 44, height: 44)
                }
                .buttonStyle(ClippySquareButtonStyle())
                .disabled(newSectionName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityLabel("Create list")
            }

            ForEach(model.library.sections) { section in
                sectionCard(section)
            }

            if model.library.sections.isEmpty {
                VStack(spacing: 10) {
                    Image(systemName: "square.stack.3d.up")
                        .font(.system(size: 26, weight: .medium))
                        .foregroundStyle(ClippyPalette.accent)
                    Text("Create your first list")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(ClippyPalette.text)
                    Text("It will appear on your Mac after the next sync.")
                        .font(.system(size: 13))
                        .foregroundStyle(ClippyPalette.muted)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 28)
                .background(ClippyPalette.surface.opacity(0.72), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            }
        }
    }

    private func sectionCard(_ section: LocalSection) -> some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .fill(ClippyPalette.accentPastelStrong)
                    .frame(width: 7, height: 22)

                Text(section.name)
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(ClippyPalette.text)
                Spacer()

                Menu {
                    Button("Rename", systemImage: "pencil") {
                        renamedSectionName = section.name
                        renamingSection = section
                    }
                    Button("Delete", systemImage: "trash", role: .destructive) {
                        model.deleteSection(id: section.id)
                    }
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(ClippyPalette.muted)
                        .frame(width: 32, height: 32)
                        .contentShape(Rectangle())
                }
                .accessibilityLabel("List options")
            }
            .padding(.horizontal, 15)
            .padding(.vertical, 13)

            Divider().overlay(ClippyPalette.hairline)

            let items = model.library.items(in: section.id)
            ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                itemRow(item)
                    .padding(.horizontal, 15)
                    .padding(.vertical, 12)
                if index < items.count - 1 {
                    Divider()
                        .overlay(ClippyPalette.hairline)
                        .padding(.leading, 49)
                }
            }

            HStack(spacing: 10) {
                Image(systemName: "plus")
                    .font(.system(size: 13, weight: .bold))
                    .foregroundStyle(ClippyPalette.accent)
                TextField("Add an item", text: Binding(
                    get: { itemDrafts[section.id, default: ""] },
                    set: { itemDrafts[section.id] = $0 }
                ), axis: .vertical)
                .submitLabel(.done)
                .onSubmit { createItem(in: section.id) }

                Button("Add") { createItem(in: section.id) }
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(ClippyPalette.accent)
                    .disabled(itemDrafts[section.id, default: ""].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .padding(.horizontal, 15)
            .padding(.vertical, 12)
            .background(ClippyPalette.field.opacity(0.78))
        }
        .background(ClippyPalette.surface)
        .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 18, style: .continuous)
                .stroke(ClippyPalette.hairline, lineWidth: 1)
        }
    }

    private func itemRow(_ item: LocalItem) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 11) {
                Button {
                    model.setItemCompleted(id: item.id, done: !item.done)
                } label: {
                    Image(systemName: item.done ? "checkmark.circle.fill" : "circle")
                        .font(.system(size: 20, weight: .medium))
                        .foregroundStyle(item.done ? ClippyPalette.accent : ClippyPalette.muted.opacity(0.72))
                }
                .buttonStyle(.plain)

                Button {
                    editingText = item.projectedContent
                    editingItem = item
                } label: {
                    Text(item.projectedContent.isEmpty ? "Empty item" : item.projectedContent)
                        .font(.system(size: 15))
                        .foregroundStyle(item.done ? ClippyPalette.muted : ClippyPalette.text)
                        .strikethrough(item.done, color: ClippyPalette.muted)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .multilineTextAlignment(.leading)
                }
                .buttonStyle(.plain)

                Menu {
                    Button("Edit", systemImage: "pencil") {
                        editingText = item.projectedContent
                        editingItem = item
                    }
                    Button("Attach file", systemImage: "paperclip") {
                        importingForItem = item.id
                        showsFileImporter = true
                    }
                    Button("Delete", systemImage: "trash", role: .destructive) {
                        model.deleteItem(id: item.id)
                    }
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(ClippyPalette.muted)
                        .frame(width: 28, height: 28)
                        .contentShape(Rectangle())
                }
                .accessibilityLabel("Item options")
            }

            if item.content.hasConflict {
                Label("Conflicting edits — choose one or merge", systemImage: "exclamationmark.triangle.fill")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(ClippyPalette.warning)

                ForEach(item.content.versions, id: \.dot) { version in
                    Button {
                        model.resolveItemConflict(id: item.id, content: version.value)
                    } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(version.value.isEmpty ? "Empty value" : version.value)
                                .foregroundStyle(ClippyPalette.text)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Text("From \(version.dot.actorId.prefix(8)) · Use this version")
                                .font(.system(size: 11, weight: .medium))
                                .foregroundStyle(ClippyPalette.muted)
                        }
                        .padding(10)
                        .background(ClippyPalette.warningPastel, in: RoundedRectangle(cornerRadius: 11, style: .continuous))
                    }
                    .buttonStyle(.plain)
                }

                Button("Merge manually") {
                    editingText = item.content.versions.map(\.value).joined(separator: "\n")
                    editingItem = item
                }
                .font(.system(size: 12, weight: .semibold))
            }

            ForEach(model.library.attachments(for: item.id)) { attachment in
                HStack(spacing: 8) {
                    Image(systemName: "doc.fill")
                        .foregroundStyle(ClippyPalette.accent)
                    Text(attachment.name)
                        .font(.system(size: 12.5, weight: .medium))
                        .foregroundStyle(ClippyPalette.muted)
                        .lineLimit(1)
                    Spacer()
                    Button("Remove", systemImage: "xmark", role: .destructive) {
                        model.deleteAttachment(id: attachment.id)
                    }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.plain)
                }
                .padding(.leading, 31)
            }
        }
    }

    private func itemEditor(_ item: LocalItem) -> some View {
        NavigationStack {
            ZStack {
                ClippyPalette.canvas.ignoresSafeArea()
                TextEditor(text: $editingText)
                    .font(.system(size: 16))
                    .scrollContentBackground(.hidden)
                    .padding(14)
                    .background(ClippyPalette.surface, in: RoundedRectangle(cornerRadius: 17, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: 17, style: .continuous)
                            .stroke(ClippyPalette.hairline, lineWidth: 1)
                    }
                    .padding(20)
            }
            .navigationTitle(item.content.hasConflict ? "Resolve conflict" : "Edit item")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { editingItem = nil }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        if item.content.hasConflict {
                            model.resolveItemConflict(id: item.id, content: editingText)
                        } else {
                            model.updateItem(id: item.id, content: editingText)
                        }
                        editingItem = nil
                    }
                    .fontWeight(.semibold)
                }
            }
        }
        .presentationDetents([.medium, .large])
        .preferredColorScheme(.light)
    }

    private func messageCard(_ message: String) -> some View {
        Label(message, systemImage: "info.circle.fill")
            .font(.system(size: 12.5, weight: .medium))
            .foregroundStyle(ClippyPalette.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(14)
            .background(ClippyPalette.surface.opacity(0.72), in: RoundedRectangle(cornerRadius: 15, style: .continuous))
    }

    private var privacyFooter: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "lock.fill")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(ClippyPalette.accent)
                .padding(.top, 1)
            Text("Your content and files are encrypted on this device before syncing.")
                .font(.system(size: 11.5, weight: .medium))
                .foregroundStyle(ClippyPalette.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, 4)
    }

    private func card<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        content()
            .padding(18)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(ClippyPalette.surface, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 20, style: .continuous)
                    .stroke(ClippyPalette.hairline, lineWidth: 1)
            }
    }

    private func createSection() {
        let name = newSectionName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return }
        model.createSection(name: name)
        newSectionName = ""
    }

    private func createItem(in sectionId: UUID) {
        let content = itemDrafts[sectionId, default: ""].trimmingCharacters(in: .whitespacesAndNewlines)
        guard !content.isEmpty else { return }
        model.createItem(sectionId: sectionId, content: content)
        itemDrafts[sectionId] = ""
    }

    private var statusText: String {
        if model.library.pendingOperationCount > 0, model.syncState != .syncing {
            return "Ready to sync"
        }
        switch model.syncState {
        case .idle: return auth.signedIn ? "Connecting your account" : "Ready when you are"
        case .syncing: return "Syncing now"
        case .synced: return "Everything is synced"
        case .waitingForDevice: return "Waiting for your Mac"
        }
    }

    private var statusDetail: String {
        if model.library.pendingOperationCount > 0, model.syncState != .syncing {
            let count = model.library.pendingOperationCount
            return "\(count) \(count == 1 ? "change" : "changes") waiting for your Mac"
        }
        switch model.syncState {
        case .idle:
            return auth.signedIn ? "Your Mac will connect automatically." : "Sign in to connect this iPhone."
        case .syncing: return "Updating your lists and files…"
        case .synced: return "Your iPhone and Mac are up to date."
        case .waitingForDevice: return "We’ll sync as soon as it’s available."
        }
    }

    private var statusSymbol: String {
        if model.library.pendingOperationCount > 0, model.syncState != .syncing {
            return "arrow.up.arrow.down"
        }
        switch model.syncState {
        case .idle: return "sparkles"
        case .syncing: return "arrow.triangle.2.circlepath"
        case .synced: return "checkmark"
        case .waitingForDevice: return "macbook"
        }
    }

    private var statusAccent: Color {
        if model.library.pendingOperationCount > 0, model.syncState != .syncing {
            return ClippyPalette.accent
        }
        switch model.syncState {
        case .idle, .waitingForDevice: return ClippyPalette.accent
        case .syncing: return ClippyPalette.warning
        case .synced: return ClippyPalette.success
        }
    }

    private var statusTint: Color { statusAccent.opacity(0.13) }
    private var statusBackground: Color { ClippyPalette.surface }
}

private enum ClippyPalette {
    static let canvas = Color(red: 220 / 255, green: 224 / 255, blue: 223 / 255)
    static let surface = Color(red: 252 / 255, green: 252 / 255, blue: 250 / 255)
    static let field = Color(red: 244 / 255, green: 246 / 255, blue: 245 / 255)
    static let text = Color(red: 32 / 255, green: 33 / 255, blue: 31 / 255)
    static let muted = Color(red: 101 / 255, green: 107 / 255, blue: 103 / 255)
    static let hairline = Color.black.opacity(0.085)
    static let accent = Color(red: 51 / 255, green: 135 / 255, blue: 232 / 255)
    static let accentPastel = Color(red: 220 / 255, green: 235 / 255, blue: 251 / 255)
    static let accentPastelStrong = Color(red: 174 / 255, green: 211 / 255, blue: 249 / 255)
    static let success = Color(red: 35 / 255, green: 139 / 255, blue: 91 / 255)
    static let warning = Color(red: 177 / 255, green: 105 / 255, blue: 26 / 255)
    static let warningPastel = Color(red: 249 / 255, green: 237 / 255, blue: 218 / 255)
    static let danger = Color(red: 190 / 255, green: 54 / 255, blue: 54 / 255)
}

private struct ClippyPrimaryButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 15, weight: .semibold))
            .foregroundStyle(isEnabled ? ClippyPalette.text : ClippyPalette.muted)
            .padding(.horizontal, 15)
            .padding(.vertical, 14)
            .frame(maxWidth: .infinity)
            .background(
                isEnabled ? ClippyPalette.accentPastel : ClippyPalette.field,
                in: RoundedRectangle(cornerRadius: 14, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .stroke(isEnabled ? ClippyPalette.accent.opacity(0.18) : ClippyPalette.hairline, lineWidth: 1)
            }
            .scaleEffect(configuration.isPressed ? 0.985 : 1)
            .opacity(configuration.isPressed ? 0.82 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}

private struct ClippySquareButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(isEnabled ? ClippyPalette.accent : ClippyPalette.muted)
            .background(
                isEnabled ? ClippyPalette.accentPastel : ClippyPalette.field,
                in: RoundedRectangle(cornerRadius: 13, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 13, style: .continuous)
                    .stroke(isEnabled ? ClippyPalette.accent.opacity(0.16) : ClippyPalette.hairline, lineWidth: 1)
            }
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}

private struct ClippyIconButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(ClippyPalette.accent)
            .scaleEffect(configuration.isPressed ? 0.93 : 1)
            .opacity(configuration.isPressed ? 0.7 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}
