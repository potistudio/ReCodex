# Pi ハーネス基盤 調査報告

## 0. 本書の位置付け

本書は `refs/pi` に固定した Pi のソースを静的に調査し、ハーネス基盤の構造、実行契約、データ所有権、拡張点、統合境界を整理したものである。採用判断そのものではなく、[独自エージェントハーネス設計案](harness-design.md) の分析事実と設計判断に根拠を与えるための資料として扱う。

記述は次の2種類に分ける。

| 区分 | 意味 |
| --- | --- |
| 分析事実 | 対象ソースまたは公式ドキュメントで確認した性質 |
| 評価 | 分析事実から導いた設計上の判断。採用・不採用の推奨を含む |

**対象**

| 項目 | 内容 |
| --- | --- |
| リポジトリ | `earendil-works/pi` |
| commit | `9376c8d17cd37f05d4f439f892026b03769aa789` |
| describe | `v1.4.0-243-g9376c8d17` |
| commit日時 | 2026-10-30 23:01:20 +0100 |
| ライセンス | MIT |

**調査方法**: パッケージ依存関係、主要ソース、設計ドキュメント、セキュリティ文書、テスト構成の静的調査。ビルド、テスト、eval、性能測定、侵入試験は未実施。

**進捗**: 静的調査と報告は 100%。実行検証は未実施のため、品質評価はコードと文書から確認できる範囲に限定する。

---

## 1. 結論

1. **Pi は単一のハーネスではない。** `pi-ai`（モデルI/O）、`pi-agent-core`（ループ）、`pi-coding-agent`（製品ハーネス）、`pi-durable`（実験的durableハーネス）の4層に分かれており、保証内容が異なる。
2. **最も再利用価値が高いのは `pi-ai` と `pi-agent-core`。** 依存DAGが明確で、TUIやセッション管理を切り離せる。`pi-coding-agent` は完成品だが、セッション、拡張、TUI、trustを一体で所有する。
3. **`pi-coding-agent` のセッション永続化は堅牢な台帳ではない。** JSONLへの同期appendで、fsync、トランザクション、複数writer排他を持たない。UIイベントは永続化より先に発火する。
4. **Pi に承認・権限境界はない。** `beforeToolCall` はフックであり、ポリシーストアでもサンドボックスでもない。拡張はPiプロセス内でフル権限実行される。
5. **`pi-durable` は設計として最も堅牢だが、実験的APIである。** 原子コミット、タスク、再開、ツールreplayを持つ一方、Chord依存、API変動、in-process plugin loaderの既知制約がある。
6. **ReCodexの正本をPiへ移してはいけない。** ReCodexのRust hostが既に持つ操作順序、承認、LAN認証、イベントreplayと役割が衝突し、二重権威になる。
7. **推奨は「ReCodexのRust制御面 + Piの実行カーネル（agent core + pi-ai）」**。Piを制御面ではなく、モデルループとツール実行の部品として使う。

---

## 2. 全体像

`refs/pi` は11のパッケージ（evalsを含む）で構成される。`src/*.ts` の概算は約218,000行。

| パッケージ | 役割 | src概算 |
| --- | --- | ---: |
| `pi-ai` | 統一LLM API、provider、認証、カタログ、stream、usage/cost | 28.6k |
| `pi-agent-core` | `Agent`、agent loop、queue、tool hooks、イベント | 2.5k |
| `pi-coding-agent` | 製品ハーネス。session、compaction、tools、extensions、TUI/RPC/SDK | 77.2k |
| `pi-durable` | 原子コミット、会話、タスク、再開を持つdurable harness | 33.3k |
| `chord` | contract、replicated state、RPC、hot reload | 45.3k |
| `pi-codemode` | QuickJS-WASMサンドボックスでモデル生成JSを実行 | 3.0k |
| `pi-mcp` | 公式SDK非依存のMCPクライアント | 3.3k |
| `pi-tui` | 差分描画のターミナルUI | 19.9k |
| `pi-env` | SSH経由のExecutionEnv | 2.4k |
| `pi-telemetry` | telemetry契約・adapter | 1.6k |
| `pi-evals` | 行動eval（private） | 1.4k |

**依存の芯**

```text
pi-ai
  ↑
pi-agent-core（依存: pi-ai, typebox のみ）
  ↑
pi-coding-agent（+ tui, codemode, mcp）
```

`pi-agent-core` がUI、セッション、拡張から独立している点は、設計上明確に肯定できる。一方 `pi-coding-agent` は `pi-tui` に依存し、拡張APIもTUI型を露出する。

主要根拠:

- `refs/pi/packages/agent/package.json`
- `refs/pi/packages/coding-agent/package.json`
- `refs/pi/README.md` のPackages表
- `refs/pi/packages/agent/README.md`

---

## 3. 実行モデル

### 3.1 1プロンプトの流れ

```text
AgentSession.prompt
  → input handlers / command / skill / prompt template 展開
  → 認証確認
  → _startRun
    → _prepareRun
       → 直前assistantがあればcompaction判定
       → before_agent_start
       → system prompt差分 + user message + 追加messageを構築
    → Agent.prompt
      → runAgentLoop
        → runLoop
          turn:
            prepareNextTurn（compaction等）
            declareToolChanges（tool宣言差分）
            prepareRequest（SessionManagerのprojectionを注入）
            streamAssistantResponse
            tool calls実行
            finishTurn
            turn_end
          steering / follow-up / 明示continuationを判定
          agent_end
    → _handlePostAgentRun（retry、compaction、queued message）
    → _runBeforeSettleBoundary
    → agent_settled
```

根拠:

- `refs/pi/packages/coding-agent/src/core/agent-session.ts:1894` `_startRun`
- `refs/pi/packages/coding-agent/src/core/agent-session.ts:1947` `_prepareRun`
- `refs/pi/packages/coding-agent/src/core/agent-session.ts:2113` `prompt`
- `refs/pi/packages/agent/src/agent-loop.ts:172` `runLoop`
- `refs/pi/packages/agent/src/agent-loop.ts:342` `declareToolChanges`
- `refs/pi/packages/agent/src/agent-loop.ts:390` `streamAssistantResponse`
- `refs/pi/packages/coding-agent/src/core/agent-session.ts:803` `prepareRequest` projection

### 3.2 用語の区別

| 用語 | 定義 | 注意点 |
| --- | --- | --- |
| turn | 1回のassistant応答 + そのtool呼び出し/結果 | モデル要求1回とは限らない |
| run（低レベル） | `agentLoop` 1回。`agent_end` で終わる | retryやqueued workで継続し得る |
| run（セッション） | prompt受付から `agent_settled` まで | `agent_end` は完了ではない |
| settle | retry、compaction、queueを含め、自動継続がなくなった状態 | UIの「完了」はこちら |

`agent_end` を完了として扱うのは誤り。`agent_settled` がセッション完了の契約。

根拠: `refs/pi/packages/coding-agent/docs/json.md`、`refs/pi/packages/coding-agent/docs/sdk.md:94`。

### 3.3 入力制御

- **steering**: 現在のassistant turnとtool実行が終わった後、次のモデル要求前に注入。
- **follow-up**: toolもsteeringもなくなった後、runが停止しようとした時に注入。
- **queue mode**: `one-at-a-time` / `all`。既定は両方 `one-at-a-time`。
- streaming中に `prompt()` を呼ぶ場合、`steer` か `followUp` の指定が必須。無指定は拒否。
- `finishTurn` は `{action:"end"}` で即終了、`{action:"continue"}` で次要求を保証する。ただしtool結果、steering、follow-upが既に次要求を発生させる場合は追加要求しない。

根拠:

- `refs/pi/packages/agent/src/types.ts:193` `AgentLoopConfig`
- `refs/pi/packages/agent/src/types.ts:264` `finishTurn`
- `refs/pi/packages/agent/src/types.ts:293` steering
- `refs/pi/packages/agent/src/types.ts:306` follow-up
- `refs/pi/packages/coding-agent/docs/sdk.md:66`

### 3.4 エラー、打ち切り、長さ超過

- `stopReason === "error" | "aborted"` はハード終了。
- `stopReason === "length"` でtool callを含む場合、引数が切れている可能性があるため、全tool callを実行せずエラー結果にする。
- providerのcontext overflowとlength停止は、compactionを挟んだ1回の再試行対象。
- retry既定: 最大3回、base delay 2秒。

根拠:

- `refs/pi/packages/agent/src/agent-loop.ts` の `failToolCallsFromTruncatedMessage`
- `refs/pi/packages/coding-agent/src/core/agent-session.ts:3032` `_checkCompaction`
- `refs/pi/packages/coding-agent/src/core/settings-defaults.ts:19`

---

## 4. コンテキストとセッション

### 4.1 system promptをメッセージとして持つ

Piの特徴は、system promptとtool宣言をtranscript内のsystem messageとして持つこと。

- 先頭system messageが基底prompt。
- 後続system messageは `sections` の名前付き差分、`toolsAdded` / `toolsRemoved` の差分。
- 全system messageを順にreplayすると、現在のpromptとtool setになる。
- providerが会話途中のsystem messageを受け付けない場合、`collapseSystemMessages` で先頭に畳み込む。

根拠:

- `refs/pi/packages/ai/src/types.ts` の `SystemMessage`
- `refs/pi/packages/ai/src/utils/transcript.ts:58` `getCurrentTools`
- `refs/pi/packages/ai/src/utils/transcript.ts:73` `getCurrentSystemMessage`
- `refs/pi/packages/ai/src/utils/transcript.ts` の `resolveTranscript`

**評価**: prompt変更とtool loadout変更を同じ差分機構で扱える点は一貫している。ただし、providerごとに「途中system messageをそのまま送るか畳むか」の差があり、デバッグ時にはモデルが実際に見たpromptをreplayで再構成する必要がある。生の `messages` を読むだけでは不十分。

### 4.2 セッションはid/parentIdの木

`SessionManager` はJSONLに以下を保存する。

| entry | 内容 |
| --- | --- |
| `session` | header。version 3、cwd、parentSession |
| `message` | user / assistant / toolResult / system / custom |
| `model_change` | モデル切替 |
| `thinking_level_change` | thinking level切替 |
| `usage` | モデル以外のusage（cache warm等） |
| `compaction` | summary、`firstKeptEntryId`、system checkpoint |
| `branch_summary` | 分岐要約 |
| `context_edit` | 射影上のmessageをomit/replace |
| `custom` / `custom_message` / `label` / `session_info` | 拡張・UI用 |

- 各entryは `id` と `parentId` を持ち、現在のleafがactive branch。
- `branch(id)` はleafを変えるだけで、捨てたbranchは削除しない。
- `createBranchedSession` は別ファイルにbranchを切り出す。
- compactionは元entryを消さず、`firstKeptEntryId` 以降だけをモデルcontextに残す。
- `ContextEditEntry` は元entryを保持したまま、モデル向け射影だけをomit/replaceする。

根拠:

- `refs/pi/packages/coding-agent/src/core/session-manager.ts:987` `SessionManager`
- `refs/pi/packages/coding-agent/src/core/session-manager.ts:476` `buildContextEntries`
- `refs/pi/packages/coding-agent/src/core/session-manager.ts:543` `buildSessionProjection`
- `refs/pi/packages/coding-agent/src/core/session-manager.ts:1589` `branch`
- `refs/pi/packages/coding-agent/src/core/session-manager.ts:1642` `createBranchedSession`
- `refs/pi/packages/coding-agent/docs/session-format.md`

**肯定できる点**: 履歴、分岐、表示、モデル入力、compactionを別のviewとして扱う設計は、監査性と再構成性が高い。raw entryを消さないため、後から別branchや別モデルで再利用できる。

**統合上の制約**: SDKは `SessionManager` を最終的なモデルcontextの権威とし、`session.agent.state.messages` の代入ではpersisted contextを置き換えないと明記している（`refs/pi/packages/coding-agent/docs/sdk.md`）。外部台帳と併用するとcontext ownershipが二重になるため、ReCodex側の台帳を正本にするなら `AgentSession` ではなく `Agent` を使う方が整合的である。

### 4.3 compaction

- 発火条件: `contextTokens > contextWindow - reserveTokens`（既定reserve 16,384）。
- 既定keepRecent 20,000 tokens。
- 古い範囲をLLMで要約し、`CompactionEntry` を追記。
- 要約後も `firstKeptEntryId` 以降のmessageとtool call/result対応は保持。
- 分割turnにも対応（turn prefix summary）。
- branch移動時はbranch summaryを使う。

根拠:

- `refs/pi/packages/coding-agent/src/core/compaction/compaction.ts:126` 既定値
- `refs/pi/packages/coding-agent/src/core/compaction/compaction.ts:267` `shouldCompact`
- `refs/pi/packages/coding-agent/src/core/compaction/compaction.ts:859` `prepareCompaction`
- `refs/pi/packages/coding-agent/src/core/compaction/compaction.ts:952` `compact`
- `refs/pi/packages/coding-agent/docs/compaction.md`

**評価**: 設計は妥当だが、要約は同じモデルへの追加リクエストであり、要約品質、コスト、遅延はモデル依存。長期タスクの状態管理を要約に依存させるべきではない。durableのタスクcheckpointの方が適切。

### 4.4 永続化の実際

`SessionManager._persist` は `appendFileSync` で1行追記する。

- fsyncなし。
- セッションファイル単位のlockなし。
- 複数entryのトランザクションなし。
- 壊れた行は読み込み時に黙ってskip。
- `_appendEntry` は書き込み失敗時にmemoryをrollbackするが、ファイル側の部分書き込みは残り得る。

根拠:

- `refs/pi/packages/coding-agent/src/core/session-manager.ts:1172` `_persist`
- `refs/pi/packages/coding-agent/src/core/session-manager.ts:1187` `appendFileSync`
- `refs/pi/packages/coding-agent/src/core/session-manager.ts:355` `parseSessionEntries`（malformed line skip）

**重要**: `SessionManager` はプロセスが正常に動いている間の履歴には十分だが、電源断、クラッシュ、複数writerに対する台帳ではない。ReCodexのLAN越し、複数クライアント、承認状態の正本には使えない。

### 4.5 イベントと永続化の順序

`_handleAgentEvent` は、extensionイベントと公開listenerへの通知を永続化より先に行う。

```text
message_end
  → extensionへemit
  → 公開listenerへemit
  → SessionManager.appendMessage
```

根拠: `refs/pi/packages/coding-agent/src/core/agent-session.ts:1158-1181`

**評価**: UIが先に更新され、その後に保存が失敗すると、クライアントが見た状態と永続状態がずれる。ReCodexのようにhostが操作順序とreplayを保証する設計では、保存してからbroadcastする必要がある。Piのイベント順序をそのまま採用してはいけない。

また、公開listenerは `_emit` で同期呼び出しされ、例外が捕捉されない。`Agent` はlistenerのPromiseをawaitするため、ホスト側listenerがthrowするとrun全体が失敗し得る。統合時はlistenerを必ずtry/catchで包むこと。

---

## 5. ツール実行

### 5.1 パイプライン

1. tool名解決
2. `prepareArguments`（互換shim）
3. TypeBox schema検証
4. `beforeToolCall`（block可能）
5. `execute`
6. `afterToolCall`（content/details/isError/usage/terminateの上書き）
7. `tool_execution_end`
8. toolResult message生成

根拠:

- `refs/pi/packages/agent/src/agent-loop.ts:718` `prepareToolCall`
- `refs/pi/packages/agent/src/agent-loop.ts:869` `finalizeExecutedToolCall`
- `refs/pi/packages/agent/src/types.ts:466` `AgentTool`

- tool失敗はthrowするか `isError: true` を返す。throwはerror tool resultになる。
- `terminate: true` はbatch内の全tool結果がtrueの時だけ次LLM要求を省略する。
- `outputSchema` / `structuredContent` により、モデル向けtextとプログラム向けstructured outputを分離できる。

### 5.2 並列実行の実態

- 既定は `parallel`。
- ただしpreflight（tool解決、引数検証、before hook）は直列。
- 許可されたtoolだけを `Promise.all` で並列実行。
- `tool_execution_end` は完了順、toolResult messageはassistantのsource順。
- batch内に1つでも `executionMode: "sequential"` のtoolがあれば、batch全体が直列。
- 同時実行数の上限はコード上見当たらない。

根拠:

- `refs/pi/packages/agent/src/agent-loop.ts:518` `executeToolCalls`
- `refs/pi/packages/agent/src/agent-loop.ts:596` `executeToolCallsParallel`
- `refs/pi/packages/agent/src/agent-loop.ts:700` `shouldTerminateToolBatch`
- `refs/pi/packages/agent/README.md` のTool execution mode

**評価**: batch単位の並列/直列しか表現できず、資源競合、排他、優先度、上限を持たない。ReCodexの複数チャット、複数クライアント、同一ファイル編集を考えると不十分。編集ツールは `withFileMutationQueue` で同一ファイルだけ直列化するが、それはプロセス内の同一パスのみ。

### 5.3 実行環境と権限

- 組み込みtool: `read`, `bash`, `powershell`, `edit`, `write`, `grep`, `find`, `ls`。
- `read` は絶対パスも受け付ける。cwdは基準点であり、境界ではない。
- `bash` はcwdを指定してspawnするが、コマンド自体はプロセス権限で何にでもアクセスできる。
- Pi全体にサンドボックス、承認、permission systemはない。
- `--tools` / `defaultTools` はモデルへの公開範囲であり、ファイルシステム制約ではない。

根拠:

- `refs/pi/packages/coding-agent/src/core/tools/index.ts`
- `refs/pi/packages/coding-agent/src/core/tools/path-utils.ts`
- `refs/pi/README.md:90`
- `refs/pi/packages/coding-agent/docs/security.md`

**評価**: ツールを無効化すれば安全、は成立しない。`bash` を残すならOSまたはコンテナ境界が必要。ReCodexの承認UIはPiの外側で、tool実行前にhostへ問い合わせる構造にすべき。

---

## 6. 拡張機構

### 6.1 読み込み

- 探索場所: `~/.pi/agent/extensions/`、`.pi/extensions/`、設定の `packages` / `extensions`、CLI `-e`。
- 1階層まで探索。`package.json` の `pi.extensions` も解決。
- TypeScriptを `jiti` でその場でロード。
- `/reload` で再読み込み。古い `ctx` はinvalidateされる。

根拠:

- `refs/pi/packages/coding-agent/src/core/extensions/loader.ts:1`
- `refs/pi/packages/coding-agent/src/core/extensions/loader.ts:570` jiti
- `refs/pi/packages/coding-agent/src/core/extensions/loader.ts:721` `loadExtensions`
- `refs/pi/packages/coding-agent/src/core/extensions/loader.ts:831` `discoverAndLoadExtensions`

### 6.2 API

`ExtensionAPI` は非常に広い。

- 約40のイベント: `session_start`, `before_agent_start`, `context`, `before_provider_request`, `tool_call`, `tool_result`, `turn_end`, `agent_before_settle`, `input`, `user_bash` 等。
- 登録: `registerTool`, `registerCommand`, `registerShortcut`, `registerFlag`, provider, virtual model, MCP server, message/entry renderer。
- 実行: `ctx.executeTool()` でネストtool呼び出し。`pi.sendMessage` / `sendUserMessage` / `appendEntry`。
- セッション操作: `newSession`, `fork`, `switchSession`, `navigateTree`, `reload`, `compact`。
- UI: `ctx.ui.select/confirm/input/editor/custom`、editor差し替え、footer/header/widget。

根拠:

- `refs/pi/packages/coding-agent/src/core/extensions/types.ts:1616` `ExtensionAPI`
- `refs/pi/packages/coding-agent/src/core/extensions/types.ts:599` `ToolDefinition`
- `refs/pi/packages/coding-agent/src/core/extensions/types.ts:164` `ExtensionUIContext`
- `refs/pi/packages/coding-agent/src/core/extensions/runner.ts:364` `ExtensionRunner`

### 6.3 信頼境界

- extensionはPiプロセス内で、Piと同じOS権限で実行される。
- project trustはプロジェクト由来の設定、extensionをロードするかのgateであり、実行サンドボックスではない。
- user-level extensionはtrust decision前からロードされ得る。
- extensionはprompt、tool call、ファイル、認証情報、セッション履歴を読める。
- `beforeToolCall` によるblockは可能だが、extension自身の任意コード実行を制限しない。

根拠:

- `refs/pi/packages/coding-agent/docs/security.md`
- `refs/pi/packages/coding-agent/docs/extensions.md`
- `refs/pi/packages/coding-agent/src/core/project-trust.ts`

**評価**: Piの拡張は信頼したコードによる自在な改造としては優秀だが、権限境界としては使えない。ReCodexがLANクライアントに拡張を許可する設計は危険。拡張を製品機能にするなら、署名、許可リスト、別プロセス隔離、ケーパビリティ制限が別途必要。

### 6.4 UI結合

`ExtensionUIContext` は `TUI`, `Component`, `EditorComponent`, `KeybindingsManager`, `Theme` を直接使う。`pi-coding-agent` は `pi-tui` に依存し、interactive-modeは約7,200行。

RPCモードでもextension UIのサブプロトコルはあるが、TUI専用機能は利用不可または縮退する。

根拠:

- `refs/pi/packages/coding-agent/package.json`
- `refs/pi/packages/coding-agent/src/core/extensions/types.ts:164`
- `refs/pi/packages/coding-agent/src/modes/interactive/interactive-mode.ts`
- `refs/pi/packages/coding-agent/docs/cli-integration.md`

**評価**: Svelte UIからPi拡張を完全互換で使うことはできない。ReCodexのUIプロトコルとPiのTUI拡張APIは分離すべき。

---

## 7. モデル抽象化

- `Models` がprovider collectionを持ち、`model.api` に応じてAPI実装へdispatchする。
- providerはcatalog、auth、stream behaviorを所有する。
- API実装はAnthropic、OpenAI、Google、Bedrock、Mistral等。OpenAI互換経由でOllama、vLLM、LM Studio等も扱う。
- `streamSimple` はthinking levelやtool choiceをprovider非依存に扱う。
- 画像生成、classifier、deferred responseも同じcollectionで扱う。
- provider SDKはlazy chunk化でき、`providers/all` は全catalogを即時importする重い入口。

根拠:

- `refs/pi/packages/ai/README.md`
- `refs/pi/packages/ai/src/models.ts`
- `refs/pi/packages/ai/src/types.ts:319` `ProviderStreams`
- `refs/pi/packages/ai/README.md:1715` Bundling

**肯定できる点**: providerの追加、認証、model metadata、usage/cost、thinking、画像/classifierまで一貫しており、自前実装の価値が最も高い部分。

**注意点**:

- `auth.json` は平文のcredential store。ファイルmode 0600と `proper-lockfile` はあるが、OS keychainではない。
- OAuth loginはNode限定。
- `providers/all` を使うとバンドルが重い。必要なproviderだけ使うなら個別factoryとlazy APIが必須。
- 認証情報をPiに持たせるか、ReCodexのRust側で持つかは別途決定が必要。

根拠:

- `refs/pi/packages/coding-agent/src/core/auth-storage.ts:34` mode 0600
- `refs/pi/packages/coding-agent/src/core/auth-storage.ts:64` `auth.json`
- `refs/pi/packages/ai/README.md:1712`

---

## 8. 統合境界

| 境界 | 実体 | 長所 | 制約 |
| --- | --- | --- | --- |
| SDK | `createAgentSession()` / `AgentSession` | in-processで全API、型付き | Node/Bun必須。session、extension、resource、TUIの既定を引き受ける |
| RPC | `pi --mode rpc` のJSONL | 言語非依存、プロセス分離 | stdioのみ。認証、TLSなし。extension UIはサブプロトコル |
| JSON | 単発 `--mode json` | 構造化イベントが簡単 | 双方向制御なし |
| Print | 最終テキスト | 最軽量 | イベントなし |
| TUI | `pi-tui` | 高機能 | ReCodexには不要。UI契約がTUI形状 |

- RPCは厳密なLF区切りJSONL。`stdout` はプロトコル専用、診断は `stderr`。
- `prompt` の成功応答は受付、queue、handleであり、完了ではない。
- `agent_settled` まで待つ必要がある。
- `RpcClient` は `node dist/cli.js --mode rpc` をspawnする。Piのビルド成果物が必要。
- RPCに認証はない。LAN公開するならReCodex側でTLS、認証、認可を被せる必要がある。

根拠:

- `refs/pi/packages/coding-agent/src/modes/rpc/rpc-mode.ts:54`
- `refs/pi/packages/coding-agent/src/modes/rpc/rpc-client.ts:56`
- `refs/pi/packages/coding-agent/docs/rpc.md`
- `refs/pi/packages/coding-agent/docs/cli-integration.md`

**評価**: ReCodexのTauri/Rust構成に対しては、RPC子プロセスが最も現実的な接続点。ただし配布時にNode 22.19以上またはBunバイナリをsidecarとして同梱する必要があり、macOSの署名/notarization、WindowsのAV誤検知、更新配布のコストが増える。SDKを直接Rustプロセスに埋め込むことはできない。

---

## 9. durable / chord

### 9.1 durableの契約

`pi-durable` はdurable agent harnessを名乗り、`pi-coding-agent` とは別の実行モデルを持つ。

- Sessionが1本のmutation lineを持つ。
- entry、task record、documentを原子的にcommitする。
- commitがstorageに永続化されるまで観測しない（deferred commitを除く）。
- 会話はimmutable entryの列。taskはphase/checkpointを持つ状態機械。
- crash後に `resume()` で未完了taskを再開。
- 同じ `requestId` の再送は同じsubmissionを返す。
- tool callはintentをcommitしてから実行。`replay: "safe"` のみ再実行。
- `maxTurns` でrunのtool roundを制限。
- 拡張はregistryで管理し、tool、section、hook、task、wrapを登録。
- codemodeはjournalでscriptのtool結果を保存し、再起動後に同じscriptを再実行して結果を再注入する。

根拠:

- `refs/pi/packages/durable/README.md`
- `refs/pi/packages/durable/docs/spec.md:44-90`
- `refs/pi/packages/durable/src/harness/generation.ts`
- `refs/pi/packages/durable/src/storage/jsonl/storage.ts`

**肯定できる点**: 表示前にcommit、taskの再開、副作用のreplay policyを明示している。`pi-coding-agent` のJSONLより一段強い保証を持つ。

**否定、注意**:

- Experimental。APIはリリース間で予告なく変わる。
- JSONL backendの `fsync` は既定false。プロセスcrashと電源断では保証が異なる。電源断まで求めるならSQLiteまたはfsync設定を別途評価する必要がある。
- Chord依存。Chordのplugin loaderはin-processの動的importで、reloadごとにモジュールコピーが残る既知のleakがある。
- ChordのWebSocket linkはReCodexの既存TLS/device認証と重複する。どちらをtransportの正本にするか決める必要がある。
- durableを採用すると、ReCodexのRust hostと二重のscheduler/ledgerを持つことになる。

根拠:

- `refs/pi/packages/durable/README.md:3`
- `refs/pi/packages/durable/src/storage/jsonl/storage.ts:247-260` `fsync` 既定false
- `refs/pi/packages/chord/docs/spec.md:7`, `:264` loader leak
- `refs/pi/packages/chord/README.md`

### 9.2 coding-agentとdurableの比較

| 観点 | coding-agent | durable |
| --- | --- | --- |
| 正本 | JSONL session | 原子的commitされたSession |
| crash recovery | なし（壊れた行skip） | あり（resume、retry、restart limit） |
| 複数writer | 想定外 | 1 mutation lineで直列化 |
| task/subagent | 標準では持たない | task graph、child task |
| tool replay | なし | `replay` policy |
| 拡張 | jitiでin-process | registryでin-process |
| UI結合 | 強い（TUI型） | 中程度（Chord/独自UI） |
| 成熟度 | 製品（1.4.0） | Experimental |

---

## 10. セキュリティと品質

### 10.1 セキュリティ

- Piはユーザー、プロセスのOS権限で動く。標準sandboxなし。
- プロジェクトtrustは起動時リソースのロード制御であり、実行制御ではない。
- prompt injectionは防がない。ファイル、コメント、コマンド出力、モデル応答がすべて入力になり得る。
- extensionはフル権限。
- codemodeはscript実行をQuickJS-WASMに閉じ込めるが、scriptが呼ぶtoolは実副作用を持つ。出力だけがモデルに返る。
- bash toolはプロセスグループ単位でkillする等の運用配慮はあるが、権限制御ではない。

根拠:

- `refs/pi/README.md:90`
- `refs/pi/packages/coding-agent/docs/security.md`
- `refs/pi/packages/codemode/README.md`
- `refs/pi/packages/coding-agent/src/core/tools/bash.ts`

### 10.2 品質、供給網

肯定できる材料:

- 857個の `*.test.ts`。
- `npm run check` にpinned deps、runtime deps、TS import、entry graph、install lock、browser smoke、tscが含まれる。
- direct dependencyはexact pin。lockfileがground truth。
- coding-agentのinstall-lockを生成、検証する。
- lifecycle script dependencyはallowlist制査。
- `pi-evals` にDocker隔離の行動evalがある。
- MIT license。

根拠:

- `refs/pi/package.json` scripts
- `refs/pi/README.md` Supply-chain hardening
- `refs/pi/packages/evals/README.md`

ただし、今回の調査ではテスト、evalを実行していない。また、agentの解決率、速度、コストをPiとDeepSeek/Codexで比較するデータはない。設計文書に性能優劣を書くべきではない。

---

## 11. ReCodexへの示唆

### 11.1 ReCodex側の既存契約

ReCodexは既にRust hostが以下を所有している。

- workspaceとプロジェクト登録
- Codex子プロセスのライフサイクル
- 操作順序
- 承認状態
- イベントのbounded replay
- LANのTLS/device認証、revocation
- ファイル保存のstale check

根拠: ReCodex `README.md` のArchitecture and scope、`src-tauri/src/host.rs`。

### 11.2 そのままPiを採用した場合の問題

| 問題 | 内容 | 代替 |
| --- | --- | --- |
| 二重権威 | ReCodex hostとPi SessionManagerが両方session、eventを持つ | Piは実行カーネルに限定し、正本はRustに置く |
| 永続性 | coding-agentのJSONLはfsync、lock、transactionなし | ReCodexのSQLite/WAL台帳に保存し、Piへは都度contextを注入 |
| 承認 | Piに承認権限がない。`beforeToolCall` はフック | Rustのpolicy/approvalをRPCで呼ぶ。Pi側はblockするだけ |
| 拡張 | in-processフル権限。LANクライアントに開放できない | 拡張はhostが署名、許可したものだけ。別プロセス隔離 |
| UI | extension APIがTUI形状 | ReCodex独自のUI protocolを定義。Pi TUIは使わない |
| 多重クライアント | coding-agentは単一writer前提 | Rust hostで直列化。Pi workerはsessionごとに分離 |
| 配布 | Node/Bun sidecarが必要 | Tauri sidecarとして同梱するか、Rust実装へ移植するか決定 |

### 11.3 推奨アーキテクチャ

**推奨: Rust制御面 + Node実行カーネル。**

```text
Tauri / Svelte UI
        │  ReCodex protocol（既存）
        ▼
Rust Host
  - workspace / session ledger（SQLite, WAL）
  - operation ordering / approval authority
  - LAN auth / event replay
  - tool policy
        │  narrow RPC（prompt, steer, abort, tool approval, tool result）
        ▼
Node Agent Worker
  - pi-agent-core の Agent（SessionManagerは使わない）
  - pi-ai の Models（必要なproviderのみ）
  - tool実装はRustのpolicy broker経由、または制限付きlocal tool
        │
        ▼
Provider API
```

**この構成の要点**:

- `Agent` の `prepareRequest` で、ReCodexの台帳から構築したcanonical contextを毎回注入する。
- `beforeToolCall` でRustへ承認要求。拒否は `{block:true, reason}`。`terminate` は使うとしても明示的に。
- tool結果はRust側で永続化してからUIへbroadcastする。Piのイベント順序をそのまま使わない。
- `finishTurn` でturn上限、予算、停止条件を強制する。
- `getSteeringMessages` / `getFollowUpMessages` はRustのqueueを読む。
- セッション分岐、compaction、context editはReCodexの台帳で管理し、Piには今回の要求に必要なcontextだけ渡す。

**この構成でPiに任せる範囲**:

- モデル要求とstreamの正規化
- assistant messageの組み立て
- tool callのschema検証と実行順序
- provider差分の吸収
- usage/costの収集

**Piに任せない範囲**:

- セッションの正本
- 承認、権限
- LAN認証
- 複数クライアントの順序
- イベントreplay
- 拡張の信頼判断

### 11.4 代替案

| 案 | 内容 | 評価 |
| --- | --- | --- |
| A. 推奨案 | Rust制御面 + `pi-agent-core` + `pi-ai` | 最も分離が良く、ReCodexの既存契約を壊さない。実装量は中 |
| B. プロトタイプ | `pi-coding-agent` をRPC sidecarで接続 | 最速で動くが、session、extension、TUIの既定を引き受ける。製品には不向き |
| C. durable採用 | `pi-durable` をsidecarで使い、Rustはtransport、authに限定 | crash recoveryとtask graphは強いが、Experimental APIとChord依存。PoC向き |
| D. 完全自前 | Rustでloop、provider、toolを実装 | 制御は完全。provider差分、stream、認証の再実装コストが大きい |
| E. Codex継続 | 既存Codex backendを維持し、Piは別backendとして追加 | 最も低リスク。`AgentBackend` traitで比較できる |

**現時点の判断**:

- 製品の正本をPiに移すのは否定する。
- まずBでPiの挙動をReCodex UIに載せ、Aのworker分離へ移行するのが現実的。
- durableはcrash recoveryが要件になった時点でCとして再評価する。今すぐ採用すると、ReCodexのhost.rsとChordのschedulerが競合する。

### 11.5 決定が必要な事項

1. **正本**: ReCodexのSQLite台帳を正本にするか、Pi sessionを正本にするか。
2. **実行配置**: Rust内でloopを実装するか、Node/Bun sidecarを許容するか。
3. **拡張**: 第三者のPi extensionを製品機能にするか。するなら隔離、署名、権限モデルが必須。
4. **durability**: crash recovery、task graphを初期要件に含めるか。
5. **認証情報**: `auth.json` をPiに持たせるか、OS keychain、Rust側のsecret storeに持たせるか。
6. **UI契約**: PiのTUI extension UIを互換対象にするか、ReCodex独自protocolに限定するか。

---

## 12. 参考資料

ローカルでは同じ相対pathを `refs/pi/` 配下で参照できる。リンクは対象commitに固定している。

### 12.1 Pi 文書

- [P7] — Pi全体像とパッケージ表: `refs/pi/README.md`
- [P8] — agent coreのループ、キュー、tool hooks、イベント: `refs/pi/packages/agent/README.md`
- [P9] — Piの実行モデル、コンテキスト、セッション: `refs/pi/packages/coding-agent/docs/how-pi-works.md`
- [P10] — SDKとセッション境界: `refs/pi/packages/coding-agent/docs/sdk.md`
- [P11] — セッションJSONL形式: `refs/pi/packages/coding-agent/docs/session-format.md`
- [P12] — compactionとbranch summary: `refs/pi/packages/coding-agent/docs/compaction.md`
- [P13] — 拡張APIとイベント: `refs/pi/packages/coding-agent/docs/extensions.md`
- [P14] — RPCプロトコル: `refs/pi/packages/coding-agent/docs/rpc.md`
- [P15] — 信頼境界とsandbox方針: `refs/pi/packages/coding-agent/docs/security.md`
- [P16] — provider、認証、stream、バンドル: `refs/pi/packages/ai/README.md`
- [P17] — durable harness: `refs/pi/packages/durable/README.md`
- [P18] — durable仕様: `refs/pi/packages/durable/docs/spec.md`
- [P19] — Chord: `refs/pi/packages/chord/README.md`
- [P20] — Chord仕様と既知制約: `refs/pi/packages/chord/docs/spec.md`
- [P21] — codemode: `refs/pi/packages/codemode/README.md`
- [P22] — MCPクライアント: `refs/pi/packages/mcp/README.md`
- [P23] — CLI、JSON、RPCの統合境界: `refs/pi/packages/coding-agent/docs/cli-integration.md`
- [P24] — eval: `refs/pi/packages/evals/README.md`

### 12.2 主要ソース

- `refs/pi/packages/agent/src/agent-loop.ts`
- `refs/pi/packages/agent/src/agent.ts`
- `refs/pi/packages/agent/src/types.ts`
- `refs/pi/packages/coding-agent/src/core/agent-session.ts`
- `refs/pi/packages/coding-agent/src/core/session-manager.ts`
- `refs/pi/packages/coding-agent/src/core/system-prompt.ts`
- `refs/pi/packages/coding-agent/src/core/extensions/types.ts`
- `refs/pi/packages/coding-agent/src/core/extensions/loader.ts`
- `refs/pi/packages/coding-agent/src/core/extensions/runner.ts`
- `refs/pi/packages/coding-agent/src/core/tools/index.ts`
- `refs/pi/packages/coding-agent/src/core/tools/file-mutation-queue.ts`
- `refs/pi/packages/coding-agent/src/modes/rpc/rpc-mode.ts`
- `refs/pi/packages/coding-agent/src/modes/rpc/rpc-client.ts`
- `refs/pi/packages/coding-agent/src/modes/interactive/interactive-mode.ts`
- `refs/pi/packages/ai/src/models.ts`
- `refs/pi/packages/ai/src/types.ts`
- `refs/pi/packages/ai/src/utils/transcript.ts`
- `refs/pi/packages/durable/src/harness/generation.ts`
- `refs/pi/packages/durable/src/storage/jsonl/storage.ts`
- `refs/pi/packages/coding-agent/src/core/auth-storage.ts`
- `refs/pi/packages/coding-agent/src/core/settings-defaults.ts`

### 12.3 固定URL

- [P7]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/README.md
- [P8]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/agent/README.md
- [P9]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/coding-agent/docs/how-pi-works.md
- [P10]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/coding-agent/docs/sdk.md
- [P11]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/coding-agent/docs/session-format.md
- [P12]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/coding-agent/docs/compaction.md
- [P13]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/coding-agent/docs/extensions.md
- [P14]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/coding-agent/docs/rpc.md
- [P15]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/coding-agent/docs/security.md
- [P16]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/ai/README.md
- [P17]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/durable/README.md
- [P18]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/durable/docs/spec.md
- [P19]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/chord/README.md
- [P20]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/chord/docs/spec.md
- [P21]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/codemode/README.md
- [P22]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/mcp/README.md
- [P23]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/coding-agent/docs/cli-integration.md
- [P24]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/evals/README.md

---

## 13. 未確認事項

- `npm run check`、`./test.sh`、`pi-evals` は未実行。
- 実モデルでの品質、速度、コスト比較は未実施。
- OSサンドボックスやLAN越しの侵入試験は未実施。
- 857個のテストの全内容はレビューしていない。
- `refs/pi` の上流最新版との差分は、このcommit以降については追跡していない。
- ReCodexの `src-tauri/src/host.rs` の全実装は本書では精読していない。READMEの契約のみ参照している。
