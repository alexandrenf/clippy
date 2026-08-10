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
            Form {
                statusSection
                accountSection

                if !model.library.actorId.isEmpty {
                    librarySections
                }

                if let message = model.message {
                    Section { Text(message).foregroundStyle(.secondary) }
                }

                Section("How sync works") {
                    Text("Changes sync while Clippy is open and both devices are available. Content and files are end-to-end encrypted before they enter the Cloudflare Tunnel.")
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Clippy")
            .toolbar {
                if !model.library.actorId.isEmpty {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("Sync", systemImage: "arrow.triangle.2.circlepath") {
                            model.syncNow()
                        }
                        .disabled(model.syncState == .syncing)
                    }
                }
            }
        }
        .sheet(item: $editingItem) { item in
            NavigationStack {
                Form {
                    TextEditor(text: $editingText)
                        .frame(minHeight: 180)
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
                    }
                }
            }
        }
        .alert("Rename section", isPresented: Binding(
            get: { renamingSection != nil },
            set: { if !$0 { renamingSection = nil } }
        )) {
            TextField("Section name", text: $renamedSectionName)
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

    private var statusSection: some View {
        Section {
            HStack {
                Circle()
                    .fill(statusColor)
                    .frame(width: 8, height: 8)
                Text(statusText)
                Spacer()
                if model.syncState == .syncing { ProgressView().controlSize(.small) }
            }
        }
    }

    @ViewBuilder
    private var accountSection: some View {
        if !auth.signedIn {
            Section("Account") {
                Button("Sign in securely") { auth.signIn() }
                if let error = auth.errorMessage {
                    Text(error).foregroundStyle(.secondary)
                }
            }
        } else if model.library.actorId.isEmpty || model.needsRelayPairing {
            Section("Pair with your Mac") {
                if model.needsRelayPairing {
                    Text("Pair once to upgrade this workspace to the secure relay connection. Your offline data stays on this iPhone.")
                        .foregroundStyle(.secondary)
                }
                TextField("Paste pairing code", text: $model.pairingCode, axis: .vertical)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button("Pair iPhone") { model.pair() }
                    .disabled(model.pairingCode.isEmpty || model.syncState == .syncing)
            }
        }
    }

    @ViewBuilder
    private var librarySections: some View {
        Section("New section") {
            HStack {
                TextField("Section name", text: $newSectionName)
                Button("Add") {
                    model.createSection(name: newSectionName)
                    newSectionName = ""
                }
                .disabled(newSectionName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }

        ForEach(model.library.sections) { section in
            Section {
                ForEach(model.library.items(in: section.id)) { item in
                    itemRow(item)
                }

                HStack {
                    TextField("Add an item", text: Binding(
                        get: { itemDrafts[section.id, default: ""] },
                        set: { itemDrafts[section.id] = $0 }
                    ), axis: .vertical)
                    Button("Add") {
                        let content = itemDrafts[section.id, default: ""]
                        model.createItem(sectionId: section.id, content: content)
                        itemDrafts[section.id] = ""
                    }
                    .disabled(itemDrafts[section.id, default: ""].isEmpty)
                }
            } header: {
                HStack {
                    Text(section.name)
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
                        Image(systemName: "ellipsis.circle")
                    }
                }
            }
        }
    }

    private func itemRow(_ item: LocalItem) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(alignment: .top, spacing: 10) {
                Button {
                    model.setItemCompleted(id: item.id, done: !item.done)
                } label: {
                    Image(systemName: item.done ? "checkmark.circle.fill" : "circle")
                        .foregroundStyle(item.done ? .green : .secondary)
                }
                .buttonStyle(.plain)

                Button {
                    editingText = item.projectedContent
                    editingItem = item
                } label: {
                    Text(item.projectedContent.isEmpty ? "Empty item" : item.projectedContent)
                        .foregroundStyle(item.done ? .secondary : .primary)
                        .strikethrough(item.done)
                        .frame(maxWidth: .infinity, alignment: .leading)
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
                }
            }

            if item.content.hasConflict {
                Label("Conflicting edits — choose one or merge", systemImage: "exclamationmark.triangle.fill")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.orange)
                ForEach(item.content.versions, id: \.dot) { version in
                    Button {
                        model.resolveItemConflict(id: item.id, content: version.value)
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(version.value.isEmpty ? "Empty value" : version.value)
                                .foregroundStyle(.primary)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Text("From \(version.dot.actorId.prefix(8)) · Use this version")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                        .padding(8)
                        .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
                    }
                    .buttonStyle(.plain)
                }
                Button("Merge manually") {
                    editingText = item.content.versions.map(\.value).joined(separator: "\n")
                    editingItem = item
                }
                .font(.caption)
            }

            ForEach(model.library.attachments(for: item.id)) { attachment in
                HStack(spacing: 7) {
                    Image(systemName: "doc")
                        .foregroundStyle(.secondary)
                    Text(attachment.name)
                        .font(.caption)
                        .lineLimit(1)
                    Spacer()
                    Button("Remove", systemImage: "xmark", role: .destructive) {
                        model.deleteAttachment(id: attachment.id)
                    }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.plain)
                }
            }
        }
        .padding(.vertical, 3)
    }

    private var statusText: String {
        if model.library.pendingOperationCount > 0, model.syncState != .syncing {
            return "Sync pending (\(model.library.pendingOperationCount))"
        }
        switch model.syncState {
        case .idle: return "Not configured"
        case .syncing: return "Syncing"
        case .synced: return "Synced"
        case .waitingForDevice: return "Waiting for Mac"
        }
    }

    private var statusColor: Color {
        if model.library.pendingOperationCount > 0, model.syncState != .syncing { return .blue }
        switch model.syncState {
        case .idle: return .secondary
        case .syncing: return .orange
        case .synced: return .green
        case .waitingForDevice: return .blue
        }
    }
}
