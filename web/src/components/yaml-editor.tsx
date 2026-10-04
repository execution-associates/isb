// A YAML code editor (CodeMirror 6, MIT): line numbers, folding, YAML
// highlighting in the app's colors (light and dark), Ctrl/Cmd-S, and the
// server's problems as markers on their lines. Loaded on demand
// (`lazy(() => import("@/components/yaml-editor"))`): it is a separate chunk,
// so the pages that never edit YAML do not download it.
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { yaml } from "@codemirror/lang-yaml";
import { bracketMatching, foldGutter, foldKeymap, HighlightStyle, indentOnInput, syntaxHighlighting } from "@codemirror/language";
import { type Diagnostic, lintGutter, setDiagnostics } from "@codemirror/lint";
import { Compartment, EditorState } from "@codemirror/state";
import { drawSelection, EditorView, highlightActiveLine, highlightActiveLineGutter, keymap, lineNumbers } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import { useEffect, useRef } from "react";
import type { Problem } from "@/lib/yaml-edit";

const theme = EditorView.theme({
  "&": {
    fontSize: "13px",
    backgroundColor: "var(--background)",
    color: "var(--foreground)",
    maxHeight: "65svh",
    minHeight: "16rem",
  },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "var(--app-font-mono)", lineHeight: "1.65", overflow: "auto", minHeight: "16rem" },
  ".cm-content": { caretColor: "var(--foreground)", padding: "10px 0" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--foreground)" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection": {
    backgroundColor: "color-mix(in oklab, var(--info) 28%, transparent)",
  },
  ".cm-gutters": { backgroundColor: "var(--muted)", color: "var(--muted-foreground)", border: "none", borderRight: "1px solid var(--border)" },
  ".cm-activeLine": { backgroundColor: "color-mix(in oklab, var(--foreground) 5%, transparent)" },
  ".cm-activeLineGutter": { backgroundColor: "color-mix(in oklab, var(--foreground) 8%, transparent)", color: "var(--foreground)" },
  ".cm-foldPlaceholder": { backgroundColor: "var(--muted)", border: "1px solid var(--border)", color: "var(--muted-foreground)" },
  ".cm-tooltip": { backgroundColor: "var(--popover)", color: "var(--popover-foreground)", border: "1px solid var(--border)", borderRadius: "6px" },
  ".cm-diagnostic-error": { borderLeftColor: "var(--destructive)" },
  ".cm-lintRange-error": { backgroundImage: "none", textDecoration: "underline wavy var(--destructive)", textUnderlineOffset: "3px" },
  ".cm-lint-marker-error": { content: '"!"' },
  ".cm-lintGutter": { width: "1.1em" },
});

const highlight = HighlightStyle.define([
  { tag: [t.propertyName, t.definition(t.propertyName)], color: "var(--info)" },
  { tag: [t.string, t.special(t.string)], color: "var(--success)" },
  { tag: [t.number, t.bool, t.null], color: "var(--warning)" },
  { tag: [t.comment, t.lineComment], color: "var(--muted-foreground)", fontStyle: "italic" },
  { tag: [t.keyword, t.labelName, t.meta], color: "var(--brand)" },
  { tag: [t.separator, t.punctuation, t.squareBracket, t.brace], color: "var(--muted-foreground)" },
  { tag: [t.typeName, t.tagName], color: "var(--brand)" },
]);

function diagnostics(state: EditorState, problems: Problem[]): Diagnostic[] {
  return problems.map((p) => {
    const line = state.doc.line(Math.min(Math.max(p.line ?? 1, 1), state.doc.lines));
    // A column places the marker on the word there; otherwise the whole line.
    const col = p.column ? Math.min(line.from + p.column - 1, line.to) : line.from;
    return { from: p.column ? col : line.from, to: p.column ? Math.max(col + 1, Math.min(line.to, col + 12)) : line.to, severity: "error", message: p.message };
  });
}

export default function YamlEditor({
  value,
  onChange,
  onSave,
  readOnly,
  problems,
  label,
}: {
  value: string;
  onChange: (v: string) => void;
  /** Ctrl/Cmd-S. */
  onSave?: () => void;
  readOnly?: boolean;
  problems?: Problem[];
  label: string;
}) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const editable = useRef(new Compartment());
  // The listeners see the latest props without rebuilding the editor.
  const cb = useRef({ onChange, onSave });
  cb.current = { onChange, onSave };

  useEffect(() => {
    const v = new EditorView({
      parent: host.current!,
      state: EditorState.create({
        doc: value,
        extensions: [
          lineNumbers(),
          highlightActiveLineGutter(),
          foldGutter(),
          lintGutter(),
          history(),
          drawSelection(),
          indentOnInput(),
          bracketMatching(),
          highlightActiveLine(),
          yaml(),
          syntaxHighlighting(highlight),
          theme,
          keymap.of([
            {
              key: "Mod-s",
              preventDefault: true,
              run: () => {
                cb.current.onSave?.();
                return true;
              },
            },
            indentWithTab,
            ...foldKeymap,
            ...historyKeymap,
            ...defaultKeymap,
          ]),
          EditorView.contentAttributes.of({ "aria-label": label, spellcheck: "false", autocapitalize: "off", autocorrect: "off" }),
          EditorView.updateListener.of((u) => {
            if (u.docChanged) cb.current.onChange(u.state.doc.toString());
          }),
          editable.current.of([EditorState.readOnly.of(!!readOnly), EditorView.editable.of(!readOnly)]),
        ],
      }),
    });
    view.current = v;
    return () => {
      v.destroy();
      view.current = null;
    };
    // Built once; the effects below follow the props.
    // eslint-disable-next-line react-hooks/exhaustive-deps -- the editor owns its document after mount
  }, []);

  // A new `value` from outside (Discard, a reload after saving) replaces the document.
  useEffect(() => {
    const v = view.current;
    if (v && v.state.doc.toString() !== value) v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: value } });
  }, [value]);

  useEffect(() => {
    view.current?.dispatch({ effects: editable.current.reconfigure([EditorState.readOnly.of(!!readOnly), EditorView.editable.of(!readOnly)]) });
  }, [readOnly]);

  useEffect(() => {
    const v = view.current;
    if (v) v.dispatch(setDiagnostics(v.state, diagnostics(v.state, problems ?? [])));
  }, [problems]);

  return <div ref={host} className="overflow-hidden rounded-lg border bg-background shadow-xs transition-[border-color,box-shadow] focus-within:border-ring focus-within:ring-[3px] focus-within:ring-ring/50" />;
}
