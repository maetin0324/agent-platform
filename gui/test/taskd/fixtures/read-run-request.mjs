// fixture の fake ワーカーが要る値だけを RunRequest（stdin の JSON 1 件）から取り出し、`sh` の eval 用に出力する。
// grep で JSON を読むと taskd 側の直列化の細部（空白の有無、フィールドの順）に依存して静かに壊れるため
// （G7-U3）、ここで一度だけきちんと解析する。taskd の crate には依存しない（docs/taskd-api-v1.md の形だけを使う）。
//
// 出力（sh の変数代入。値はシングルクォートで囲む）:
//   TITLE          task.title
//   TASK_ID        task.id
//   KIND           task.kind（execute / plan / approval / review）
//   ROLE           context.role.id（無ければ空）
//   INSTRUCTIONS   context.role.instructions（無ければ空）
//   CHILDREN       context.children の件数
//   CHILD_TITLES   context.children[].title を空白区切りで
let raw = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  raw += chunk;
});
process.stdin.on("end", () => {
  let request;
  try {
    request = JSON.parse(raw);
  } catch {
    request = {};
  }
  const task = request.task ?? {};
  const context = request.context ?? {};
  const children = Array.isArray(context.children) ? context.children : [];
  const quote = (value) => `'${String(value ?? "").replaceAll("'", `'\\''`)}'`;
  const lines = [
    `TITLE=${quote(task.title)}`,
    `TASK_ID=${quote(task.id)}`,
    `KIND=${quote(task.kind)}`,
    `ROLE=${quote(context.role?.id)}`,
    `INSTRUCTIONS=${quote(context.role?.instructions)}`,
    `CHILDREN=${quote(children.length)}`,
    `CHILD_TITLES=${quote(children.map((child) => child.title ?? "").join(" "))}`,
  ];
  process.stdout.write(`${lines.join("\n")}\n`);
});
