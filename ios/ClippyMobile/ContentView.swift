import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    @ObservedObject var model: AppModel
    @ObservedObject var auth: AuthController
    @Environment(\.colorScheme) private var colorScheme

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
                    LazyVStack(alignment: .leading, spacing: ClippySpace.l) {
                        if !auth.signedIn {
                            signedOutContent
                        } else if model.library.actorId.isEmpty {
                            statusBanner
                            accountConnectionCard
                        } else {
                            statusBanner
                            librarySections
                        }

                        if let message = model.message {
                            messageCard(message)
                        }

                        privacyFooter
                    }
                    .padding(.horizontal, ClippySpace.m)
                    .padding(.top, ClippySpace.s)
                    .padding(.bottom, ClippySpace.xl)
                }
                .scrollIndicators(.hidden)
            }
            .navigationTitle("Clippy")
            .navigationBarTitleDisplayMode(.inline)
            .toolbarColorScheme(colorScheme, for: .navigationBar)
            .toolbar {
                if auth.signedIn {
                    if !model.library.actorId.isEmpty {
                        ToolbarItem(placement: .primaryAction) {
                            Button {
                                model.syncNow()
                            } label: {
                                Image(systemName: "arrow.triangle.2.circlepath")
                            }
                            .disabled(model.syncState == .syncing)
                            .accessibilityLabel("Sync now")
                        }
                    }

                    ToolbarItem(placement: .topBarTrailing) {
                        Menu {
                            Button("Sign out", systemImage: "rectangle.portrait.and.arrow.right") {
                                auth.signOut()
                            }
                        } label: {
                            Image(systemName: "ellipsis")
                        }
                        .accessibilityLabel("Account options")
                    }
                }
            }
        }
        .tint(ClippyPalette.accent)
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

    private var statusBanner: some View {
        HStack(spacing: ClippySpace.s) {
            Image(systemName: statusSymbol)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(statusAccent)

            Text(statusText)
                .font(ClippyType.captionMedium)
                .foregroundStyle(ClippyPalette.text)

            Text("·")
                .foregroundStyle(ClippyPalette.tertiary)

            Text(statusDetail)
                .font(ClippyType.caption)
                .foregroundStyle(ClippyPalette.muted)
                .lineLimit(1)

            Spacer(minLength: ClippySpace.xs)

            if model.syncState == .syncing {
                ProgressView()
                    .controlSize(.small)
                    .tint(statusAccent)
            }
        }
        .padding(.horizontal, ClippySpace.m)
        .frame(minHeight: 44)
        .modifier(ClippyStatusSurface(tint: statusTint))
        .accessibilityElement(children: .combine)
    }

    private var signedOutContent: some View {
        VStack(alignment: .leading, spacing: ClippySpace.xl) {
            deviceConnectionMark

            VStack(alignment: .leading, spacing: ClippySpace.s) {
                Text("One account. Every list.")
                    .font(ClippyType.display)
                    .tracking(-0.5)
                    .foregroundStyle(ClippyPalette.text)
                Text("Sign in with the email you use on your Mac. Clippy connects your devices automatically.")
                    .font(ClippyType.body)
                    .foregroundStyle(ClippyPalette.muted)
                    .lineSpacing(4)
                    .fixedSize(horizontal: false, vertical: true)
            }

            signInButton

            if let error = auth.errorMessage {
                Label(error, systemImage: "exclamationmark.circle.fill")
                    .font(ClippyType.captionMedium)
                    .foregroundStyle(ClippyPalette.danger)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Label("Magic link only — no password stored", systemImage: "lock.fill")
                .font(ClippyType.caption)
                .foregroundStyle(ClippyPalette.muted)
        }
        .padding(.top, ClippySpace.xl)
    }

    private var deviceConnectionMark: some View {
        HStack(spacing: ClippySpace.m) {
            deviceMark(symbol: "macbook", label: "Mac")

            HStack(spacing: ClippySpace.xs) {
                Circle()
                    .frame(width: 5, height: 5)
                Rectangle()
                    .frame(height: 1)
                Circle()
                    .frame(width: 5, height: 5)
            }
            .foregroundStyle(ClippyPalette.accentPastelStrong)
            .frame(maxWidth: .infinity)
            .accessibilityHidden(true)

            deviceMark(symbol: "iphone", label: "iPhone")
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Clippy connects your Mac and iPhone")
    }

    private func deviceMark(symbol: String, label: String) -> some View {
        VStack(spacing: ClippySpace.s) {
            Image(systemName: symbol)
                .font(.system(size: 24, weight: .medium))
                .foregroundStyle(ClippyPalette.accent)
                .frame(width: 56, height: 56)
                .background(ClippyPalette.accentPastel, in: RoundedRectangle(cornerRadius: ClippyRadius.l, style: .continuous))
            Text(label)
                .font(ClippyType.captionMedium)
                .foregroundStyle(ClippyPalette.muted)
        }
    }

    private var accountConnectionCard: some View {
        card {
            VStack(alignment: .leading, spacing: ClippySpace.m) {
                VStack(alignment: .leading, spacing: ClippySpace.s) {
                    Text("Finding your Mac")
                        .font(ClippyType.heading)
                        .foregroundStyle(ClippyPalette.text)
                    Text("Keep Clippy open there for a moment. Your lists will appear here without a pairing code.")
                        .font(ClippyType.body)
                        .foregroundStyle(ClippyPalette.muted)
                        .lineSpacing(4)
                        .fixedSize(horizontal: false, vertical: true)
                }

                HStack(spacing: ClippySpace.s) {
                    ProgressView()
                        .controlSize(.small)
                        .tint(ClippyPalette.accent)
                    Text(model.connectingAccount ? "Looking…" : "Waiting for your Mac…")
                        .font(ClippyType.captionMedium)
                        .foregroundStyle(ClippyPalette.muted)
                }
            }
        }
    }

    private var librarySections: some View {
        VStack(alignment: .leading, spacing: ClippySpace.m) {
            HStack(alignment: .firstTextBaseline) {
                Text("Lists")
                    .font(ClippyType.sectionTitle)
                    .tracking(-0.4)
                    .foregroundStyle(ClippyPalette.text)
                Spacer()
                Text("\(model.library.sections.count) \(model.library.sections.count == 1 ? "list" : "lists")")
                    .font(ClippyType.caption)
                    .foregroundStyle(ClippyPalette.muted)
            }

            HStack(spacing: ClippySpace.s) {
                TextField("New list name", text: $newSectionName)
                    .font(ClippyType.body)
                    .textInputAutocapitalization(.sentences)
                    .submitLabel(.done)
                    .onSubmit(createSection)
                    .padding(.horizontal, ClippySpace.m)
                    .frame(minHeight: 52)
                    .background(ClippyPalette.paper, in: RoundedRectangle(cornerRadius: ClippyRadius.m, style: .continuous))
                    .shadow(color: ClippyPalette.shadow, radius: 8, y: 3)

                createSectionButton
            }

            ForEach(model.library.sections) { section in
                sectionCard(section)
            }

            if model.library.sections.isEmpty {
                ContentUnavailableView {
                    Label("No lists yet", systemImage: "list.bullet.clipboard")
                        .foregroundStyle(ClippyPalette.text)
                } description: {
                    Text("Name one above. It will appear on your Mac automatically.")
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, ClippySpace.l)
            }
        }
    }

    private func sectionCard(_ section: LocalSection) -> some View {
        VStack(spacing: 0) {
            HStack(spacing: ClippySpace.s) {
                Image(systemName: "list.bullet")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(ClippyPalette.accent)
                    .frame(width: 28, height: 28)
                    .background(ClippyPalette.accentPastel, in: RoundedRectangle(cornerRadius: ClippyRadius.s, style: .continuous))

                Text(section.name)
                    .font(ClippyType.subheading)
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
                        .frame(width: 44, height: 44)
                        .contentShape(Rectangle())
                }
                .accessibilityLabel("List options")
            }
            .padding(.leading, ClippySpace.m)
            .padding(.trailing, ClippySpace.s)
            .padding(.vertical, ClippySpace.s)

            Divider().overlay(ClippyPalette.hairline)

            let items = model.library.items(in: section.id)
            ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                itemRow(item)
                    .padding(.horizontal, ClippySpace.m)
                    .padding(.vertical, ClippySpace.s)
                if index < items.count - 1 {
                    Divider()
                        .overlay(ClippyPalette.hairline)
                        .padding(.leading, 49)
                }
            }

            HStack(spacing: ClippySpace.s) {
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
                    .font(ClippyType.captionMedium)
                    .foregroundStyle(ClippyPalette.accent)
                    .frame(minWidth: 44, minHeight: 44)
                    .disabled(itemDrafts[section.id, default: ""].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .padding(.horizontal, ClippySpace.m)
            .padding(.vertical, ClippySpace.xs)
            .background(ClippyPalette.field)
        }
        .background(ClippyPalette.paper)
        .clipShape(RoundedRectangle(cornerRadius: ClippyRadius.l, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: ClippyRadius.l, style: .continuous)
                .stroke(ClippyPalette.hairline.opacity(0.75), lineWidth: 0.5)
        }
    }

    private func itemRow(_ item: LocalItem) -> some View {
        VStack(alignment: .leading, spacing: ClippySpace.s) {
            HStack(alignment: .top, spacing: ClippySpace.s) {
                Button {
                    model.setItemCompleted(id: item.id, done: !item.done)
                } label: {
                    Image(systemName: item.done ? "checkmark.circle.fill" : "circle")
                        .font(.system(size: 20, weight: .medium))
                        .foregroundStyle(item.done ? ClippyPalette.accent : ClippyPalette.muted.opacity(0.72))
                        .frame(width: 44, height: 44)
                }
                .buttonStyle(.plain)

                Button {
                    editingText = item.projectedContent
                    editingItem = item
                } label: {
                    Text(item.projectedContent.isEmpty ? "Empty item" : item.projectedContent)
                        .font(ClippyType.body)
                        .foregroundStyle(item.done ? ClippyPalette.muted : ClippyPalette.text)
                        .strikethrough(item.done, color: ClippyPalette.muted)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .multilineTextAlignment(.leading)
                        .frame(minHeight: 44, alignment: .leading)
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
                        .frame(width: 44, height: 44)
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
                        .padding(ClippySpace.s)
                        .background(ClippyPalette.warningPastel, in: RoundedRectangle(cornerRadius: ClippyRadius.s, style: .continuous))
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
                    .padding(.leading, 44)
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
                    .background(ClippyPalette.paper, in: RoundedRectangle(cornerRadius: ClippyRadius.l, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: 17, style: .continuous)
                            .stroke(ClippyPalette.hairline, lineWidth: 1)
                    }
                    .padding(ClippySpace.m)
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
    }

    private func messageCard(_ message: String) -> some View {
        Label(message, systemImage: "info.circle.fill")
            .font(ClippyType.caption)
            .foregroundStyle(ClippyPalette.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(ClippySpace.m)
            .background(ClippyPalette.field, in: RoundedRectangle(cornerRadius: ClippyRadius.m, style: .continuous))
    }

    private var privacyFooter: some View {
        HStack(alignment: .top, spacing: ClippySpace.s) {
            Image(systemName: "lock.fill")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(ClippyPalette.accent)
                .padding(.top, 1)
            Text("End-to-end encrypted before sync")
                .font(ClippyType.footnote)
                .foregroundStyle(ClippyPalette.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, ClippySpace.xs)
    }

    private func card<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        content()
            .padding(ClippySpace.l)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(ClippyPalette.paper, in: RoundedRectangle(cornerRadius: ClippyRadius.l, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: ClippyRadius.l, style: .continuous)
                    .stroke(ClippyPalette.hairline.opacity(0.75), lineWidth: 0.5)
            }
    }

    @ViewBuilder
    private var signInButton: some View {
        if #available(iOS 26.0, *) {
            Button(action: auth.signIn) {
                signInLabel
                    .padding(.horizontal, ClippySpace.m)
                    .frame(minHeight: 52)
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.glassProminent)
            .tint(ClippyPalette.accent)
        } else {
            Button(action: auth.signIn) {
                signInLabel
            }
            .buttonStyle(ClippyPrimaryButtonStyle())
        }
    }

    private var signInLabel: some View {
        HStack(spacing: 9) {
            Image(systemName: "envelope.fill")
            Text("Continue with email")
            Spacer()
            Image(systemName: "arrow.right")
        }
    }

    @ViewBuilder
    private var createSectionButton: some View {
        if #available(iOS 26.0, *) {
            Button(action: createSection) {
                Image(systemName: "plus")
                    .font(.system(size: 16, weight: .bold))
                    .frame(width: 52, height: 52)
            }
            .buttonStyle(.glassProminent)
            .tint(ClippyPalette.accent)
            .disabled(newSectionName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .accessibilityLabel("Create list")
        } else {
            Button(action: createSection) {
                Image(systemName: "plus")
                    .font(.system(size: 16, weight: .bold))
                    .frame(width: 52, height: 52)
            }
            .buttonStyle(ClippySquareButtonStyle())
            .disabled(newSectionName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .accessibilityLabel("Create list")
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
    static let canvas = Color(light: 0xF3F0EA, dark: 0x171817)
    static let paper = Color(light: 0xFFFDF9, dark: 0x252624)
    static let surface = paper
    static let field = Color(light: 0xECE9E2, dark: 0x2E302D)
    static let text = Color(light: 0x1C1D1B, dark: 0xF5F2EB)
    static let muted = Color(light: 0x676A65, dark: 0xA9ACA5)
    static let tertiary = Color(light: 0xA19F98, dark: 0x747770)
    static let hairline = Color(light: 0xDEDAD1, dark: 0x3A3C38)
    static let shadow = Color.black.opacity(0.055)
    static let accent = Color(light: 0x3478B8, dark: 0x82B7E8)
    static let accentPastel = Color(light: 0xE2EDF7, dark: 0x263A4B)
    static let accentPastelStrong = Color(light: 0xB9D3EB, dark: 0x3B6486)
    static let success = Color(light: 0x287B55, dark: 0x69C798)
    static let warning = Color(light: 0xA8641C, dark: 0xE6A45F)
    static let warningPastel = Color(light: 0xF6E9D6, dark: 0x493621)
    static let danger = Color(light: 0xB93D3D, dark: 0xEF7B78)
}

private enum ClippyType {
    static let display = Font.system(size: 30, weight: .bold, design: .serif)
    static let sectionTitle = display
    static let heading = Font.system(size: 17, weight: .semibold)
    static let subheading = heading
    static let body = Font.system(size: 16)
    static let caption = Font.system(size: 12)
    static let captionMedium = Font.system(size: 12, weight: .semibold)
    static let footnote = caption
}

private enum ClippySpace {
    static let xs: CGFloat = 4
    static let s: CGFloat = 8
    static let m: CGFloat = 16
    static let l: CGFloat = 24
    static let xl: CGFloat = 32
}

private enum ClippyRadius {
    static let s: CGFloat = 8
    static let m: CGFloat = 12
    static let l: CGFloat = 16
}

private extension Color {
    init(light: UInt32, dark: UInt32) {
        self.init(uiColor: UIColor { traits in
            let value = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(
                red: CGFloat((value >> 16) & 0xFF) / 255,
                green: CGFloat((value >> 8) & 0xFF) / 255,
                blue: CGFloat(value & 0xFF) / 255,
                alpha: 1
            )
        })
    }
}

private struct ClippyPrimaryButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(ClippyType.body.weight(.semibold))
            .foregroundStyle(isEnabled ? Color.white : ClippyPalette.muted)
            .padding(.horizontal, ClippySpace.m)
            .frame(minHeight: 52)
            .frame(maxWidth: .infinity)
            .background(
                isEnabled ? ClippyPalette.accent : ClippyPalette.field,
                in: RoundedRectangle(cornerRadius: ClippyRadius.m, style: .continuous)
            )
            .scaleEffect(configuration.isPressed ? 0.98 : 1)
            .opacity(configuration.isPressed ? 0.82 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}

private struct ClippySquareButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(isEnabled ? Color.white : ClippyPalette.muted)
            .background(
                isEnabled ? ClippyPalette.accent : ClippyPalette.field,
                in: RoundedRectangle(cornerRadius: ClippyRadius.m, style: .continuous)
            )
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}

private struct ClippyStatusSurface: ViewModifier {
    let tint: Color

    @ViewBuilder
    func body(content: Content) -> some View {
        if #available(iOS 26.0, *) {
            content.glassEffect(.regular.tint(tint), in: .capsule)
        } else {
            content.background(tint, in: Capsule())
        }
    }
}
