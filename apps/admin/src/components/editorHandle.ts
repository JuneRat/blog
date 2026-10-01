/** The media workflow inserts through the active editor, including contenteditable IR. */
export interface EditorImage { url: string; alt: string }
export interface MarkdownEditorHandle {
  insertImages: (images: EditorImage[], replaceSelection: boolean) => string;
}
