import PhotosUI
import QuickLook
import SwiftUI
import UniformTypeIdentifiers
import UIKit

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
    @State private var photoForItem: UUID?
    @State private var selectedPhoto: PhotosPickerItem?
    @State private var showsPhotoPicker = false
    @State private var cameraForItem: UUID?
    @State private var showsCamera = false
    @State private var attachmentError: String?
    @State private var preparingAttachmentID: UUID?
    @State private var previewingAttachment: PreparedAttachment?
    @State private var showsSignOutConfirmation = false
    @AppStorage("clippy.mobile.showCompletedItems") private var showsCompletedItems = true
    @AppStorage("clippy.mobile.completedItemsLast") private var putsCompletedItemsLast = true
    @AppStorage("clippy.mobile.listDensity") private var listDensityRaw = MobileListDensity.comfortable.rawValue
    @AppStorage("clippy.mobile.hideSyncedBanner") private var hidesSyncedBanner = false

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
                            if showsStatusBanner {
                                statusBanner
                            }
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
                            Section("View") {
                                Toggle(isOn: $showsCompletedItems) {
                                    Label("Show completed", systemImage: "checkmark.circle")
                                }
                                Toggle(isOn: $putsCompletedItemsLast) {
                                    Label("Completed items last", systemImage: "arrow.down.to.line")
                                }
                                Picker("Row spacing", selection: $listDensityRaw) {
                                    ForEach(MobileListDensity.allCases) { density in
                                        Text(density.label).tag(density.rawValue)
                                    }
                                }
                                Toggle(isOn: $hidesSyncedBanner) {
                                    Label("Hide banner when synced", systemImage: "rectangle.compress.vertical")
                                }
                            }

                            Divider()

                            Button("Sign out", systemImage: "rectangle.portrait.and.arrow.right") {
                                showsSignOutConfirmation = true
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
        .confirmationDialog(
            "Sign out of Clippy on this iPhone?",
            isPresented: $showsSignOutConfirmation,
            titleVisibility: .visible
        ) {
            Button("Sign out on this iPhone", role: .destructive) {
                model.signOut()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Your local lists and workspace key will stay on this device.")
        }
        .sheet(item: $editingItem) { item in
            itemEditor(item)
        }
        .sheet(item: $previewingAttachment) { attachment in
            AttachmentPreviewSheet(attachment: attachment) {
                model.discardAttachmentPreview(attachment)
                previewingAttachment = nil
            }
            .onDisappear {
                model.discardAttachmentPreview(attachment)
            }
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
        .photosPicker(
            isPresented: $showsPhotoPicker,
            selection: $selectedPhoto,
            matching: .images,
            photoLibrary: .shared()
        )
        .onChange(of: selectedPhoto) { _, photo in
            guard let photo, let itemId = photoForItem else { return }
            selectedPhoto = nil
            photoForItem = nil

            Task {
                do {
                    guard let data = try await photo.loadTransferable(type: Data.self) else {
                        throw PhotoImportError.missingData
                    }
                    let type = photo.supportedContentTypes.first(where: { $0.conforms(to: .image) }) ?? .jpeg
                    model.addAttachment(
                        itemId: itemId,
                        name: "Photo-\(Self.photoTimestamp.string(from: Date())).\(type.preferredFilenameExtension ?? "jpg")",
                        mediaType: type.preferredMIMEType ?? "image/jpeg",
                        data: data
                    )
                } catch {
                    attachmentError = "That photo could not be imported."
                }
            }
        }
        .sheet(isPresented: $showsCamera, onDismiss: {
            cameraForItem = nil
        }) {
            CameraCaptureView { data in
                defer {
                    showsCamera = false
                    cameraForItem = nil
                }
                guard let data, let itemId = cameraForItem else { return }
                model.addAttachment(
                    itemId: itemId,
                    name: "Photo-\(Self.photoTimestamp.string(from: Date())).jpg",
                    mediaType: "image/jpeg",
                    data: data
                )
            }
            .ignoresSafeArea()
        }
        .alert("Attachment unavailable", isPresented: Binding(
            get: { attachmentError != nil },
            set: { if !$0 { attachmentError = nil } }
        )) {
            Button("OK", role: .cancel) { attachmentError = nil }
        } message: {
            Text(attachmentError ?? "Please try again.")
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
            let inboxItems = displayedItems(model.library.inboxItems)
            if !inboxItems.isEmpty {
                inboxCard(inboxItems)
            }

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
                    Label(
                        model.library.inboxItems.isEmpty ? "No lists yet" : "No named lists yet",
                        systemImage: "list.bullet.clipboard"
                    )
                        .foregroundStyle(ClippyPalette.text)
                } description: {
                    Text("Name one above. It will appear on your Mac automatically.")
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, ClippySpace.l)
            }
        }
    }

    private func inboxCard(_ items: [LocalItem]) -> some View {
        VStack(spacing: 0) {
            HStack(spacing: ClippySpace.s) {
                Image(systemName: "tray.fill")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(ClippyPalette.accent)
                    .frame(width: 28, height: 28)
                    .background(
                        ClippyPalette.accentPastel,
                        in: RoundedRectangle(cornerRadius: ClippyRadius.s, style: .continuous)
                    )

                Text("Inbox")
                    .font(ClippyType.subheading)
                    .foregroundStyle(ClippyPalette.text)
                Spacer()
                Text("\(items.count)")
                    .font(ClippyType.captionMedium)
                    .foregroundStyle(ClippyPalette.muted)
            }
            .padding(.horizontal, ClippySpace.m)
            .padding(.vertical, ClippySpace.s)

            Divider().overlay(ClippyPalette.hairline)

            ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                itemRow(item)
                    .padding(.horizontal, ClippySpace.m)
                    .padding(.vertical, itemRowVerticalPadding)
                if index < items.count - 1 {
                    Divider()
                        .overlay(ClippyPalette.hairline)
                        .padding(.leading, 49)
                }
            }
        }
        .background(ClippyPalette.paper)
        .clipShape(RoundedRectangle(cornerRadius: ClippyRadius.l, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: ClippyRadius.l, style: .continuous)
                .stroke(ClippyPalette.hairline.opacity(0.75), lineWidth: 0.5)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Inbox")
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

            let allItems = model.library.items(in: section.id)
            let items = displayedItems(allItems)
            ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                itemRow(item)
                    .padding(.horizontal, ClippySpace.m)
                    .padding(.vertical, itemRowVerticalPadding)
                if index < items.count - 1 {
                    Divider()
                        .overlay(ClippyPalette.hairline)
                        .padding(.leading, 49)
                }
            }

            let hiddenCompleted = allItems.filter(\.done).count
            if !showsCompletedItems, hiddenCompleted > 0 {
                Button {
                    showsCompletedItems = true
                } label: {
                    Label(
                        "Show \(hiddenCompleted) completed",
                        systemImage: "checkmark.circle"
                    )
                    .font(ClippyType.captionMedium)
                    .foregroundStyle(ClippyPalette.muted)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .frame(minHeight: 44)
                }
                .buttonStyle(.plain)
                .padding(.horizontal, ClippySpace.m)
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
                        .lineLimit(listDensity == .compact ? 2 : nil)
                        .frame(minHeight: 44, alignment: .leading)
                }
                .buttonStyle(.plain)

                Menu {
                    Button("Edit", systemImage: "pencil") {
                        editingText = item.projectedContent
                        editingItem = item
                    }
                    Menu("Add attachment", systemImage: "paperclip") {
                        Button("Choose Photo", systemImage: "photo.on.rectangle") {
                            photoForItem = item.id
                            showsPhotoPicker = true
                        }
                        Button("Take Photo", systemImage: "camera") {
                            guard UIImagePickerController.isSourceTypeAvailable(.camera) else {
                                attachmentError = "The camera is not available on this device."
                                return
                            }
                            cameraForItem = item.id
                            showsCamera = true
                        }
                        Button("Choose File", systemImage: "folder") {
                            importingForItem = item.id
                            showsFileImporter = true
                        }
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
                HStack(spacing: ClippySpace.xs) {
                    Button {
                        previewAttachment(attachment)
                    } label: {
                        HStack(spacing: ClippySpace.s) {
                            Image(systemName: attachment.mediaType.hasPrefix("image/") ? "photo.fill" : "doc.fill")
                                .font(.system(size: 14, weight: .semibold))
                                .foregroundStyle(ClippyPalette.accent)
                                .frame(width: 32, height: 32)
                                .background(
                                    ClippyPalette.accentPastel,
                                    in: RoundedRectangle(cornerRadius: ClippyRadius.s, style: .continuous)
                                )

                            VStack(alignment: .leading, spacing: 2) {
                                Text(attachment.name)
                                    .font(.system(size: 12.5, weight: .semibold))
                                    .foregroundStyle(ClippyPalette.text)
                                    .lineLimit(1)
                                Text(attachmentDetail(attachment))
                                    .font(ClippyType.footnote)
                                    .foregroundStyle(ClippyPalette.muted)
                                    .lineLimit(1)
                            }

                            Spacer(minLength: ClippySpace.xs)

                            if preparingAttachmentID == attachment.id {
                                ProgressView()
                                    .controlSize(.small)
                            } else {
                                Image(systemName: "chevron.right")
                                    .font(.system(size: 11, weight: .semibold))
                                    .foregroundStyle(ClippyPalette.tertiary)
                            }
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .disabled(preparingAttachmentID != nil)

                    Button("Remove", systemImage: "xmark", role: .destructive) {
                        model.deleteAttachment(id: attachment.id)
                    }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.plain)
                    .foregroundStyle(ClippyPalette.muted)
                    .frame(width: 44, height: 44)
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

    private var listDensity: MobileListDensity {
        MobileListDensity(rawValue: listDensityRaw) ?? .comfortable
    }

    private var itemRowVerticalPadding: CGFloat {
        listDensity == .compact ? ClippySpace.xs : ClippySpace.s
    }

    private var showsStatusBanner: Bool {
        !hidesSyncedBanner || !model.isFullySynced || model.library.pendingOperationCount > 0
    }

    private func displayedItems(_ items: [LocalItem]) -> [LocalItem] {
        let visible = showsCompletedItems ? items : items.filter { !$0.done }
        guard putsCompletedItemsLast else { return visible }
        return visible.enumerated()
            .sorted { left, right in
                if left.element.done != right.element.done {
                    return !left.element.done
                }
                return left.offset < right.offset
            }
            .map(\.element)
    }

    private func previewAttachment(_ attachment: LocalAttachment) {
        guard preparingAttachmentID == nil else { return }
        preparingAttachmentID = attachment.id
        Task {
            defer { preparingAttachmentID = nil }
            do {
                previewingAttachment = try await model.prepareAttachmentPreview(id: attachment.id)
            } catch {
                attachmentError = attachment.mediaType.hasPrefix("image/")
                    ? "This image is not available on this iPhone yet. Sync once more and try again."
                    : "This attachment is not available on this iPhone yet. Sync once more and try again."
            }
        }
    }

    private func attachmentDetail(_ attachment: LocalAttachment) -> String {
        let kind = attachment.mediaType.hasPrefix("image/") ? "Image" : "Attachment"
        guard let size = attachment.size else { return "\(kind) · Tap to preview" }
        return "\(kind) · \(ByteCountFormatter.string(fromByteCount: Int64(size), countStyle: .file))"
    }

    private var statusText: String {
        if model.isFullySynced, model.library.pendingOperationCount == 0 {
            return "Everything is synced"
        }
        switch model.syncState {
        case .idle: return auth.signedIn ? "Connecting your account" : "Ready when you are"
        case .syncing: return "Syncing now"
        case .synced: return "Checking for updates"
        case .waitingForDevice:
            return model.library.actorId.isEmpty ? "Waiting for your Mac" : "Sync paused"
        }
    }

    private var statusDetail: String {
        if model.isFullySynced, model.library.pendingOperationCount == 0 {
            if let date = model.lastSuccessfulSyncAt {
                return "Updated \(Self.relativeDate.localizedString(for: date, relativeTo: Date()))."
            }
            return "Your encrypted workspace is up to date."
        }
        if model.library.pendingOperationCount > 0 {
            let count = model.library.pendingOperationCount
            return "\(count) \(count == 1 ? "change" : "changes") still uploading"
        }
        switch model.syncState {
        case .idle:
            return auth.signedIn ? "Your Mac will connect automatically." : "Sign in to connect this iPhone."
        case .syncing: return "Updating your lists and files…"
        case .synced: return "Verifying the latest Convex frontier…"
        case .waitingForDevice:
            return model.library.actorId.isEmpty
                ? "Keep Clippy open on another signed-in device."
                : "We’ll retry automatically when the connection is available."
        }
    }

    private var statusSymbol: String {
        if model.isFullySynced, model.library.pendingOperationCount == 0 {
            return "checkmark"
        }
        switch model.syncState {
        case .idle: return "sparkles"
        case .syncing: return "arrow.triangle.2.circlepath"
        case .synced: return "checkmark"
        case .waitingForDevice:
            return model.library.actorId.isEmpty ? "macbook" : "wifi.exclamationmark"
        }
    }

    private var statusAccent: Color {
        if model.isFullySynced, model.library.pendingOperationCount == 0 {
            return ClippyPalette.success
        }
        switch model.syncState {
        case .idle, .waitingForDevice: return ClippyPalette.accent
        case .syncing: return ClippyPalette.warning
        case .synced: return ClippyPalette.accent
        }
    }

    private var statusTint: Color { statusAccent.opacity(0.13) }
    private var statusBackground: Color { ClippyPalette.surface }

    private static let photoTimestamp: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyyMMdd-HHmmss"
        return formatter
    }()

    private static let relativeDate: RelativeDateTimeFormatter = {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        return formatter
    }()
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

private enum PhotoImportError: Error {
    case missingData
}

private enum MobileListDensity: String, CaseIterable, Identifiable {
    case comfortable
    case compact

    var id: String { rawValue }
    var label: String { self == .comfortable ? "Comfortable" : "Compact" }
}

private struct AttachmentPreviewSheet: View {
    let attachment: PreparedAttachment
    let onClose: () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            AttachmentQuickLook(url: attachment.url)
                .background(ClippyPalette.canvas)
                .navigationTitle(attachment.name)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Done") {
                            onClose()
                            dismiss()
                        }
                    }
                    ToolbarItem(placement: .primaryAction) {
                        ShareLink(item: attachment.url) {
                            Image(systemName: "square.and.arrow.up")
                        }
                        .accessibilityLabel("Share or open attachment")
                    }
                }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
    }
}

private struct AttachmentQuickLook: UIViewControllerRepresentable {
    let url: URL

    func makeCoordinator() -> Coordinator {
        Coordinator(url: url)
    }

    func makeUIViewController(context: Context) -> QLPreviewController {
        let controller = QLPreviewController()
        controller.dataSource = context.coordinator
        return controller
    }

    func updateUIViewController(_ controller: QLPreviewController, context: Context) {
        guard context.coordinator.url != url else { return }
        context.coordinator.url = url
        controller.reloadData()
    }

    final class Coordinator: NSObject, QLPreviewControllerDataSource {
        var url: URL

        init(url: URL) {
            self.url = url
        }

        func numberOfPreviewItems(in controller: QLPreviewController) -> Int { 1 }

        func previewController(
            _ controller: QLPreviewController,
            previewItemAt index: Int
        ) -> QLPreviewItem {
            url as NSURL
        }
    }
}

private struct CameraCaptureView: UIViewControllerRepresentable {
    let onComplete: (Data?) -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(onComplete: onComplete)
    }

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let controller = UIImagePickerController()
        controller.sourceType = .camera
        controller.cameraCaptureMode = .photo
        controller.mediaTypes = [UTType.image.identifier]
        controller.delegate = context.coordinator
        return controller
    }

    func updateUIViewController(_ uiViewController: UIImagePickerController, context: Context) {}

    final class Coordinator: NSObject, UINavigationControllerDelegate, UIImagePickerControllerDelegate {
        private let onComplete: (Data?) -> Void

        init(onComplete: @escaping (Data?) -> Void) {
            self.onComplete = onComplete
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            onComplete(nil)
        }

        func imagePickerController(
            _ picker: UIImagePickerController,
            didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]
        ) {
            let image = info[.originalImage] as? UIImage
            onComplete(image?.jpegData(compressionQuality: 0.92))
        }
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
