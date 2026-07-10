import type { Editor, Range } from "@tiptap/core";

export type SlashCommandGroup =
  | "structure"
  | "blocks"
  | "callout"
  | "renderers"
  | "refs"
  | "dates"
  | "utilities";

export type SlashCommand = {
  id: string;
  label: string;
  hint: string;
  aliases: string[];
  indicator: string;
  group: SlashCommandGroup;
  run: (editor: Editor, range: Range) => void;
};

const GROUP_ORDER: SlashCommandGroup[] = [
  "structure",
  "blocks",
  "callout",
  "renderers",
  "refs",
  "dates",
  "utilities",
];

const GROUP_LABEL: Record<SlashCommandGroup, string> = {
  structure: "Structure",
  blocks: "Blocks",
  callout: "Callouts",
  renderers: "Renderers",
  refs: "Refs",
  dates: "Dates",
  utilities: "Utilities",
};

function todayIso(): string {
  const d = new Date();
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

function nowHm(): string {
  const d = new Date();
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

export const SLASH_COMMANDS: SlashCommand[] = [
  {
    id: "paragraph",
    label: "Paragraph",
    hint: "Plain text",
    aliases: ["p", "paragraph", "texto", "text"],
    indicator: "¶",
    group: "structure",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).setParagraph().run(),
  },
  {
    id: "h1",
    label: "Heading 1",
    hint: "Main title",
    aliases: ["h1", "heading1", "titulo1", "t1"],
    indicator: "H1",
    group: "structure",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).toggleHeading({ level: 1 }).run(),
  },
  {
    id: "h2",
    label: "Heading 2",
    hint: "Section title",
    aliases: ["h2", "heading2", "titulo2", "t2"],
    indicator: "H2",
    group: "structure",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).toggleHeading({ level: 2 }).run(),
  },
  {
    id: "h3",
    label: "Heading 3",
    hint: "Subsection",
    aliases: ["h3", "heading3", "titulo3", "t3"],
    indicator: "H3",
    group: "structure",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).toggleHeading({ level: 3 }).run(),
  },
  {
    id: "h4",
    label: "Heading 4",
    hint: "Sub-sub",
    aliases: ["h4", "heading4"],
    indicator: "H4",
    group: "structure",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).toggleHeading({ level: 4 }).run(),
  },
  {
    id: "h5",
    label: "Heading 5",
    hint: "Deep hierarchy",
    aliases: ["h5", "heading5"],
    indicator: "H5",
    group: "structure",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).toggleHeading({ level: 5 }).run(),
  },
  {
    id: "h6",
    label: "Heading 6",
    hint: "Eyebrow",
    aliases: ["h6", "heading6"],
    indicator: "H6",
    group: "structure",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).toggleHeading({ level: 6 }).run(),
  },
  {
    id: "bullet",
    label: "List",
    hint: "Bullet list",
    aliases: ["bullet", "list", "lista", "ul"],
    indicator: "•",
    group: "structure",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).toggleBulletList().run(),
  },
  {
    id: "ordered",
    label: "Ordered list",
    hint: "Automatic numbering",
    aliases: ["ordered", "ol", "numbered", "numerada"],
    indicator: "1.",
    group: "structure",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).toggleOrderedList().run(),
  },
  {
    id: "task",
    label: "Task",
    hint: "Checkbox",
    aliases: ["task", "todo", "tarefa", "check"],
    indicator: "[ ]",
    group: "structure",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).toggleTaskList().run(),
  },
  {
    id: "quote",
    label: "Quote",
    hint: "Blockquote",
    aliases: ["quote", "blockquote", "citacao"],
    indicator: "❝",
    group: "blocks",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).toggleBlockquote().run(),
  },
  {
    id: "code",
    label: "Code block",
    hint: "Syntax highlight",
    aliases: ["code", "codigo", "pre"],
    indicator: "<>",
    group: "blocks",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).toggleCodeBlock().run(),
  },
  {
    id: "math",
    label: "Math block",
    hint: "LaTeX via KaTeX",
    aliases: ["math", "latex", "formula", "tex"],
    indicator: "Σ",
    group: "blocks",
    run: (editor, range) =>
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent({ type: "blockMath", attrs: { latex: "" } })
        .run(),
  },
  {
    id: "query",
    label: "Database view (query)",
    hint: "Insert live {{query <expr>}} (table view)",
    aliases: ["query", "db", "database", "dataview"],
    indicator: "{}",
    group: "blocks",
    run: (editor, range) => {
      const expr = window.prompt(
        "Query expression (ex: (and (todo TODO))):",
        "(and (todo TODO))",
      );
      if (!expr) return;
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent({ type: "queryBlock", attrs: { expr: expr.trim() } })
        .run();
    },
  },
  {
    id: "embed-page",
    label: "Page embed",
    hint: "Transclude content from another page",
    aliases: ["embed", "embedpage", "transclusao", "transclude"],
    indicator: "⤴",
    group: "blocks",
    run: (editor, range) => {
      const name = window.prompt("Page name to embed:");
      if (!name || !name.trim()) return;
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent({
          type: "embedBlock",
          attrs: {
            targetKind: "page",
            targetName: name.trim(),
            targetUuid: "",
          },
        })
        .run();
    },
  },
  {
    id: "divider",
    label: "Divider",
    hint: "Horizontal line",
    aliases: ["divider", "hr", "rule", "linha"],
    indicator: "—",
    group: "blocks",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).setHorizontalRule().run(),
  },
  {
    id: "table",
    label: "Table",
    hint: "Initial 3x3",
    aliases: ["table", "tabela", "grid"],
    indicator: "▦",
    group: "blocks",
    run: (editor, range) =>
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertTable({ rows: 3, cols: 3, withHeaderRow: true })
        .run(),
  },
  {
    id: "image",
    label: "Image (URL)",
    hint: "Paste an external URL",
    aliases: ["image", "img", "imagem", "picture"],
    indicator: "▢",
    group: "blocks",
    run: (editor, range) => {
      const url = window.prompt("Image URL:");
      if (!url) return;
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .setImage({ src: url })
        .run();
    },
  },
  {
    id: "callout-note",
    label: "Callout: Note",
    hint: "Informational box",
    aliases: ["note", "info", "callout"],
    indicator: "ℹ",
    group: "callout",
    run: (editor, range) =>
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .setCallout("note")
        .run(),
  },
  {
    id: "callout-tip",
    label: "Callout: Tip",
    hint: "Dica destacada",
    aliases: ["tip", "dica", "callout"],
    indicator: "💡",
    group: "callout",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).setCallout("tip").run(),
  },
  {
    id: "callout-important",
    label: "Callout: Important",
    hint: "Destaque",
    aliases: ["important", "importante", "callout"],
    indicator: "❗",
    group: "callout",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).setCallout("important").run(),
  },
  {
    id: "callout-warning",
    label: "Callout: Warning",
    hint: "Aviso",
    aliases: ["warning", "aviso", "callout"],
    indicator: "⚠",
    group: "callout",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).setCallout("warning").run(),
  },
  {
    id: "callout-caution",
    label: "Callout: Caution",
    hint: "Caution",
    aliases: ["caution", "perigo", "danger", "callout"],
    indicator: "🚨",
    group: "callout",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).setCallout("caution").run(),
  },
  {
    id: "mermaid",
    label: "Diagram (Mermaid)",
    hint: "Flowcharts, sequence, Gantt - Mermaid syntax",
    aliases: ["mermaid", "diagram", "diagrama", "flow", "fluxo"],
    indicator: "▦",
    group: "renderers",
    run: (editor, range) =>
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent({
          type: "mermaid",
          attrs: { source: "graph LR;\n  A-->B" },
        })
        .run(),
  },
  {
    id: "flowchart",
    label: "Flowchart (flowchart.js)",
    hint: "flowchart.js syntax: start/end/operation/condition symbols",
    aliases: ["flowchart", "fluxograma", "fluxo", "fc"],
    indicator: "⇄",
    group: "renderers",
    run: (editor, range) =>
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent({
          type: "flowchart",
          attrs: {
            source:
              "st=>start: Start\nop=>operation: Step\ne=>end: End\nst->op->e",
          },
        })
        .run(),
  },
  {
    id: "mindmap",
    label: "Mind map (markmap)",
    hint: "Indented Markdown: # root, ## branch, ### leaf",
    aliases: ["mindmap", "markmap", "mapa", "mental", "brain"],
    indicator: "⌘",
    group: "renderers",
    run: (editor, range) =>
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent({
          type: "mindmap",
          attrs: {
            source: "# Root\n## Branch A\n### Leaf 1\n### Leaf 2\n## Branch B",
          },
        })
        .run(),
  },
  {
    id: "abc",
    label: "Score (ABC notation)",
    hint: "ABC music notation - scale, melody, harmony",
    aliases: ["abc", "musica", "music", "score", "partitura", "notation"],
    indicator: "♪",
    group: "renderers",
    run: (editor, range) =>
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent({
          type: "abc",
          attrs: {
            source: "X:1\nT:Scale\nM:4/4\nL:1/4\nK:C\nC D E F | G A B c |",
          },
        })
        .run(),
  },
  {
    id: "plantuml",
    label: "UML diagram (PlantUML)",
    hint: "Sequence, class, use cases - PlantUML syntax",
    aliases: ["plantuml", "puml", "uml", "diagrama", "sequence", "class"],
    indicator: "⚙",
    group: "renderers",
    run: (editor, range) =>
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent({
          type: "plantuml",
          attrs: {
            source: "@startuml\nAlice -> Bob: hello\nBob --> Alice: hi\n@enduml",
          },
        })
        .run(),
  },
  {
    id: "link",
    label: "Link",
    hint: "[text](url)",
    aliases: ["link", "url", "a"],
    indicator: "🔗",
    group: "refs",
    run: (editor, range) => {
      const url = window.prompt("URL:");
      if (!url) return;
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent(`[${url}](${url})`)
        .run();
    },
  },
  {
    id: "page-link",
    label: "Page link [[",
    hint: "Start a page link (autocomplete in D1)",
    aliases: ["page", "pagina", "wiki", "wikilink"],
    indicator: "[[",
    group: "refs",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).insertContent("[[").run(),
  },
  {
    id: "block-ref",
    label: "Block ref ((",
    hint: "Start a block ref (autocomplete in D1)",
    aliases: ["ref", "block", "bloco"],
    indicator: "((",
    group: "refs",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).insertContent("((").run(),
  },
  {
    id: "tag",
    label: "Tag #",
    hint: "Start a hashtag",
    aliases: ["tag", "hashtag", "etiqueta"],
    indicator: "#",
    group: "refs",
    run: (editor, range) =>
      editor.chain().focus().deleteRange(range).insertContent("#").run(),
  },
  {
    id: "today",
    label: "Today (text)",
    hint: "Insert YYYY-MM-DD",
    aliases: ["today", "hoje", "date", "data"],
    indicator: "📅",
    group: "dates",
    run: (editor, range) =>
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent(`${todayIso()} `)
        .run(),
  },
  {
    id: "today-link",
    label: "Today (link)",
    hint: "Insert [[YYYY-MM-DD]]",
    aliases: ["todaylink", "datelink", "datalink"],
    indicator: "📆",
    group: "dates",
    run: (editor, range) =>
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent(`[[${todayIso()}]] `)
        .run(),
  },
  {
    id: "now",
    label: "Now (HH:MM)",
    hint: "Insert current time",
    aliases: ["now", "agora", "hora", "time"],
    indicator: "⏱",
    group: "dates",
    run: (editor, range) =>
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent(`${nowHm()} `)
        .run(),
  },
  {
    id: "clear",
    label: "Clear formatting",
    hint: "Remove marks from the selection",
    aliases: ["clear", "limpar", "unset"],
    indicator: "⌫",
    group: "utilities",
    run: (editor, range) =>
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .unsetAllMarks()
        .clearNodes()
        .run(),
  },
  {
    id: "property",
    label: "Property",
    hint: "Insert key:: value line",
    aliases: ["property", "prop", "propriedade", "kv"],
    indicator: "::",
    group: "utilities",
    run: (editor, range) => {
      const key = window.prompt("Property key:");
      if (!key || !key.trim()) return;
      const cleanKey = key.trim().replace(/[^a-zA-Z0-9_-]/g, "_");
      const value = window.prompt(`Value for ${cleanKey}:`) ?? "";
      editor
        .chain()
        .focus()
        .deleteRange(range)
        .insertContent(`${cleanKey}:: ${value}`)
        .run();
    },
  },
  {
    id: "template",
    label: "Apply template",
    hint: "Open templates page",
    aliases: ["template", "tpl", "templates"],
    indicator: "▤",
    group: "utilities",
    run: (editor, range) => {
      editor.chain().focus().deleteRange(range).run();
      window.location.href = "/study/notes/templates";
    },
  },
];

export type FilteredSlashGroup = {
  group: SlashCommandGroup;
  label: string;
  items: SlashCommand[];
};

export function filterSlashCommands(query: string): FilteredSlashGroup[] {
  const q = query.trim().toLowerCase();
  const matches = q
    ? SLASH_COMMANDS.filter((c) => {
        if (c.label.toLowerCase().includes(q)) return true;
        if (c.id.toLowerCase().includes(q)) return true;
        if (c.aliases.some((a) => a.toLowerCase().includes(q))) return true;
        return false;
      })
    : SLASH_COMMANDS.slice();
  const grouped: Record<string, SlashCommand[]> = {};
  for (const c of matches) {
    (grouped[c.group] = grouped[c.group] || []).push(c);
  }
  const out: FilteredSlashGroup[] = [];
  for (const g of GROUP_ORDER) {
    if (grouped[g] && grouped[g].length > 0) {
      out.push({ group: g, label: GROUP_LABEL[g], items: grouped[g] });
    }
  }
  return out;
}
