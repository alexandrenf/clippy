export interface Section {
  id: number;
  name: string;
}

export interface Attachment {
  id: number;
  name: string;
  path: string;
  mediaType: string;
  size: number;
}

export interface AttachmentDraft {
  path: string;
  name: string;
  mediaType: string;
  size: number;
  preview: string | null;
  temporary: boolean;
}

export interface Item {
  id: number;
  sectionId: number | null;
  content: string;
  done: boolean;
  createdAt: number;
  updatedAt: number;
  attachments: Attachment[];
}

export type Theme = "system" | "light" | "dark" | "glass";

export interface AppState {
  sections: Section[];
  items: Item[];
  activeSectionId: number | null;
  theme: Theme;
  keepOnTop: boolean;
  showShortcut: string;
  captureShortcut: string;
}
