# 独自エージェントハーネス設計案

## 要旨

**推奨するのは「小さな固定カーネル＋交換可能な能力アダプター」である。** Piの実行核の分離と、DeepSeek Harnessの能力境界・永続ログ・実行ガードを採用する。どちらかの製品全体をフォークして合体させる構成は採らない。

ReCodexではSvelte UI、Rustホスト、LAN認証を残し、TypeScript製の独立ランタイムをサイドカーとして追加する。独自に所有するのは実行状態機械、セッション形式、コンテキスト構築、実行ポリシーとの契約であり、モデル通信まで全面的に再実装する必要はない。

重要な発見として、**DeepSeek Harness自身も、複数プロバイダー対応にPiの`pi-ai`を使うアダプターを持つ。** 両者は完全に独立した代替製品ではない。「モデル通信を再利用し、ハーネスを独自設計する」方針には既存の実例がある。ただしDeepSeek側には依存版へのパッチもあり、無条件に同等の互換性が得られるわけではない。[D7]

初期版では単一エージェント、実行上限、再接続、承認、障害復旧に集中する。汎用プラグイン基盤、HMR、自律的なエージェントチーム、ベクトル記憶は後回しにする。

## 1. 分析対象と限界

| 対象             | 固定したソース                                                      |
| ---------------- | ------------------------------------------------------------------- |
| Pi Agent         | `refs/pi`、`9376c8d17cd37f05d4f439f892026b03769aa789`               |
| DeepSeek Harness | `refs/deepseek-harness`、`ce7073d51254c26cd63b22a60835b8f948deb447` |
| ReCodex          | 現在の`README.md`、`src-tauri/src/host.rs`、プロジェクト構成        |

公式ドキュメントに加え、Piの`runLoop`・リクエスト変換・並列ツール実行、DeepSeekの`ReactLoopAgent.step`・`buildRequest`・`ToolExecutionPipeline.prepare`を確認した。[P1][P2][D1][D2][D3]

以下では、既存実装で確認した事実と、新規に提案する仕様を分ける。実モデルを使った比較、負荷試験、サンドボックスの侵入試験は行っていない。速度、費用、解決率について優劣を実証したものではない。

前提は「ReCodexをローカル中心・Windows対応・LANから操作可能な開発環境として発展させる」ことである。クラウドの不特定多数向けマルチテナントサービスは対象外とする。

## 2. 比較結果

| 観点         | Pi Agent                                               | DeepSeek Harness                                          | 独自設計への判断                                    |
| ------------ | ------------------------------------------------------ | --------------------------------------------------------- | --------------------------------------------------- |
| 基本構造     | `pi-ai`、`pi-agent-core`、coding-agent層を分離         | Cordis上でモデル・ループ・保存までプラグイン化            | 小さな核を固定し、必要な能力だけ交換可能にする      |
| 実行単位     | coreのturnは概ねモデル要求1回＋ツール実行              | stepがモデル要求＋ツール、turnは複数stepを含む            | 独自プロトコルはrun / step / attemptを明示する      |
| 入力制御     | steeringとfollow-upを別タイミングで消費                | inboxを永続イベントから投影し、次step入力と待機入力を管理 | 中断と追加指示を区別し、受付済み入力を永続化する    |
| セッション   | JSONLの`id` / `parentId`による分岐木                   | append-onlyログとモデル向けsurfaceの投影                  | 元ログを保存し、表示とモデル入力を派生させる        |
| コンテキスト | active branch、圧縮、branch summary、context edit      | ログから要求を構成し、header・context・置換を記録         | 実際に送った意味的要求のスナップショットを残す      |
| ツール       | schema検証、before/after hook、直列・並列              | executor、承認、monotonic guard、結果確定を分離           | 拒否を後段で解除できない固定ガードを置く            |
| 並列実行     | core既定は並列。直列指定のツールがあればバッチを直列化 | 並列上限とexclusive barrierを持つ                         | 読み取りだけ限定並列。書き込み・shellは初期版で直列 |
| 拡張         | ツール・イベント・リソース・プロバイダーの拡張         | service依存、scoped registration、破棄可能なeffect        | 明示的な登録とdisposeだけ先に用意する               |
| 外部統合     | in-process SDK、JSON、長寿命RPC                        | 名前付きprofileを起動するstdio JSON-RPC SDKなど           | UI非依存のバージョン付きサイドカープロトコル        |
| セキュリティ | ユーザー権限で動作。標準サンドボックスなし             | sandbox / approval seamあり。ただし未監査のpreview        | OS隔離を別の信頼境界として実装・検証する            |
| 委譲         | 製品の標準機能を最小化し、拡張で追加する方針           | in-process、Codex、Claude Code、ACP等の委譲provider       | 初期版は不要。必要になった時点で子runを追加         |

根拠: [P1][P2][P3][P4][P5][P6][D1][D3][D4][D5][D6][D8][D9][D10]。

### 2.1 Piから採用するもの

- **UIを必要としない実行核。** `AgentMessage`とモデル向けmessageを変換する境界があり、UI独自データをそのままモデルAPIへ流さない。[P1][P2]
- **モデル通信の独立性。** モデルAPIの差異と、タスク実行・履歴管理を別の層に置ける。
- **steer / follow-upの意味の分離。** 追加指示は現在のツール処理後、後続タスクは自然停止後に入れる。即時停止はabortとして別扱いにする。[P1]
- **履歴を消さないコンテキスト削減。** 圧縮やcontext editは将来のモデル入力を変え、元の履歴を残す。ツール呼び出しと結果を分断しない。[P3][P4]
- **同じエンジンを複数の入口から使う。** デスクトップ、CLI、テストで別々のループを作らない。[P5]

そのまま採用しないのは、同一ユーザー権限を前提とする信頼モデルである。LANから操作されるReCodexで、拡張のフックだけを実行防御とみなすのは不十分である。Pi公式も標準サンドボックスを意図的に持たないと明記している。[P6]

### 2.2 DeepSeekから採用するもの

- **能力の定義・実装・利用側の分離。** 例えばfilesystemとsubprocessを同じ実行環境へ向ければ、ツールごとにSSH版を作らずに済む。[D1]
- **model-visible means logged。** 追加コンテキスト、ツール定義、モデル設定の変更を、再構成可能な事実として扱う。[D1][D2]
- **観測用イベントと制御用処理の区別。** UI通知の購読者に実行可否を決めさせない。永続イベントと一時的なstreamも分ける。[D1]
- **承認と解除不能なガードの分離。** 承認されてもguardが拒否すれば実行しない。承認者不在やキャンセルで実行を許可しない。[D3][D4]
- **失敗した試行と成功したメッセージの分離。** 中断・失敗を履歴から消さず、モデルへ再送する内容と区別する。[D1][D2]
- **結果不明の副作用を自動再実行しない。** 呼び出し開始記録はあるが結果がないケースを、未開始と区別する。[D2]

全面採用しないのは、すべてをCordisのplugin treeと多層patchで構成する方式である。実際に交換したい箇所が少ない初期段階では、依存解決・ロード順序・破棄順序・設定レイヤーまで製品仕様になる。ReCodexでは明示的な構成関数で開始する。

また、DeepSeekのドキュメントにはroot shutdown時の終端イベント永続化の制約と、組み込みturn budgetがないことが記されている。設計上の参考にはなるが、そのまま運用保証として利用してはならない。[D2]

### 2.3 安全性についての結論

DeepSeekのsandboxは`read-only`等のファイル効果を対象とし、ネットワークやプロセス可視性は同じモードの保証に含まれない。Windows ACL backendや古いLandlock ABIでは`partial`を報告する。`workspace-write`という名前だけで「外部通信も機密の読み取りも防げる」と解釈するのは誤りである。[D5]

さらに公式に、未監査でproduction-readyではなく、唯一の安全対策として依存すべきでないと明記されている。[D6]

**プロンプト、承認ダイアログ、プロセス分離、OSサンドボックスはそれぞれ別の対策であり、互いの代替ではない。**

## 3. 推奨アーキテクチャ

```text
Svelte UI / CLI / LAN client
            |
            v
Rust Host
  device authentication / project authorization / child lifecycle
  approval authority / execution broker / sandbox capability probe
            |
            | versioned JSON-RPC over stdio
            v
TypeScript Harness Runtime
  Run Controller ---- Context Builder ---- Model Adapter
       |                    |                   |
  Tool Scheduler       Request Snapshot       pi-ai
       |                    |              or native adapter
       +---------- Session Journal -----------+
       |
       | execution request / approval request
       v
Rust Execution Broker
  guarded filesystem / confined subprocess / output artifacts
```

Rust HostとExecution Brokerは同じホストプロセス内の別モジュールとして開始する。初期版から別サービスへ分割しない。上図の相互呼び出しは、同じstdio接続で相関IDを付けた双方向要求として扱う。

### 3.1 所有権を固定する

| 領域                              | 唯一の所有者      | 他層に許可しないこと                        |
| --------------------------------- | ----------------- | ------------------------------------------- |
| 独自sessionのログ・モデル向け投影 | Harness Runtime   | UIやRustからのDB直接更新                    |
| run状態、入力queue、モデル試行    | Run Controller    | adapterやpluginによる状態の直接変更         |
| デバイス認証・共有projectの権限   | Rust Host         | clientが送ったcwdや許可フラグを信頼すること |
| OS実行の承認・隔離・実行receipt   | Execution Broker  | runtime側のallowだけで実行すること          |
| Codex sessionと資格情報           | 既存Codex backend | 独自ランタイムによる内部形式の書き換え      |
| 表示状態・未送信入力              | UI                | 表示の都合で永続履歴を書き換えること        |

ランタイムは信頼された自社コードとして扱う。モデルはツール要求を生成するだけで、承認主体にはならない。悪意ある自社ランタイムまで封じ込める設計ではない。

### 3.2 固定カーネルの責務

固定するのは次の6点に限定する。

1. run / step / attemptの状態遷移と受付済み入力の管理。
2. 永続イベントの順序、単一writer、schema version。
3. モデル要求の確定と、履歴からの投影規則。
4. ツール呼び出しと終端結果の対応、実行上限とキャンセル。
5. brokerへ渡す変更不能な要求と、その結果の検証。
6. protocolのバージョン交渉、snapshotと再購読。

初期状態は`idle → preparing → generating → executing / awaiting-approval → generating → settled`とし、各活動から`cancelling → cancelled`または`failed`へ遷移できるようにする。ツール結果が存在するだけで無制限に継続させず、次stepの開始前に必ずbudgetを確認する。

- `run`: ユーザー入力で起動する一連の自動処理。
- `step`: モデル要求と、それに応じたツール処理。
- `attempt`: 同一step内の通信再試行。provider・モデル・要求snapshotを識別する。

### 3.3 最初に必要な交換境界

- **ModelAdapter**: 対応能力、要求の事前検証、stream、usage、provider固有replay情報。
- **ExecutionBackend**: 同じ実行環境のfilesystemとsubprocess。初期実装はRust brokerを使うlocalだけ。
- **ContextPolicy**: 保持範囲、出力参照化、圧縮を提案する。ログへの確定はkernelが行う。
- **ToolDefinition**: 入出力schema、効果分類、実行要求の組み立て。OS操作を直接持たせない。

初期版の保存はSQLite実装に固定し、まだ存在しないクラウドDB用pluginを作らない。UI rendererもkernelのpluginにしない。

ModelAdapterの最初の実装には`pi-ai`を候補とする。ただし外部へ公開する型は独自型とし、依存版とadapter versionを固定する。モデルのcontext長、tool support、画像、reasoning、system更新の対応を明示し、未対応指定は事前に拒否する。OpenAI互換という理由で全機能互換とは扱わない。

## 4. セッション、コンテキスト、復旧

### 4.1 永続化の最小仕様

独自sessionはSQLite WALのイベント表を正本とする。単一runtimeがwriterを所有し、別のruntimeによる同一ストアの起動はlockで拒否する。JSONLは監査・移植用exportとして提供する。

イベントには少なくとも`schemaVersion`、`eventId`、`sessionId`、単調増加`sessionSeq`、`runId`、必要に応じて`stepId` / `attemptId` / `callId`、時刻を持たせる。分岐は新sessionから元sessionの確定済みseqを参照する。初期版ではPiのような同一ファイル内の自由な分岐木UIまでは作らない。

主要な事実は次とする。

- 入力の受付・claim・取り消し、runの開始・終端。
- モデル要求snapshotと、そのsource event範囲・policy / adapter version。
- assistantの確定message、失敗または中断attempt。
- toolの要求、dispatch受付、終端結果。
- 承認の要求・決定、適用policy version。
- compactionの開始・成功または失敗、置換対象と生成summary。
- token使用量と費用の推定。再試行・圧縮も集計対象とする。

要求snapshotにはモデルへ実際に渡したsystem / messages / tool schema / generation設定と、必要なartifact参照を保存する。画像やprovider固有の変換も、再構成に必要な版情報と結果を記録する。認証headerやAPI keyは保存しない。ここで保証するのは**認証情報を除く意味的要求の再構成**であり、外部モデルの同一回答やHTTP全体のbit単位再現ではない。

### 4.2 一時streamと確定状態

- text deltaやツール進捗は一時通知とし、全deltaごとのDB同期は要求しない。
- 確定messageは永続commit後に通知し、UIはそれを正本として途中表示を置き換える。
- プロセスが確定前に落ちた場合、未commitの表示prefixは失われ得る。再接続時に「未確定出力」と明示して確定snapshotへ合わせる。
- `settled`はログのflushと進行中ツールの状態確定後に発行する。単にモデルstreamが終了しただけでは発行しない。
- 遅いUIのためにrunを停止しない。購読queueを有界化し、溢れたclientにはsnapshot再取得を要求する。

これにより「UIが見たすべての文字が必ず永続化済み」という重い保証を初期版で約束せず、確定状態の整合性を優先する。

### 4.3 正常時の実行順序

1. Hostでclientのproject権限を検証する。
2. Runtimeが入力とcommand IDを永続化してから受付応答する。
3. Context Builderがモデル入力を構成し、能力検証と必要な圧縮を行う。
4. 要求snapshotをcommitし、ModelAdapterを呼ぶ。
5. 完成したassistant messageをcommitする。不完全なJSON引数は実行しない。
6. 各tool要求をcommitし、brokerへ渡す。
7. Brokerが固定guard、必要な承認、実行直前の再検証を行う。
8. 実行receiptと結果を受け、Runtimeがtool終端結果をcommitする。
9. budgetと入力queueを検査し、次stepまたはrun終端へ進む。

MCPや将来のcodemode内の入れ子呼び出しも、同じtool pipelineとbrokerを通す。便利な実行経路を安全性の例外にしない。

### 4.4 障害時にしてはいけないこと

**モデル再試行と、副作用を持つツールの再実行を一緒にしない。**

Brokerは`callId`と要求hashに対する受付・実行結果receiptを専用journalに保存する。このjournalはOS操作の証跡であり、会話ログの第二writerではない。Runtimeは復旧時にreceiptを照会して会話ログへ取り込む。

| 障害点                   | 復旧動作                                                  |
| ------------------------ | --------------------------------------------------------- |
| 入力受付応答の通信断     | 同じcommand IDの再送に同じ受付結果を返す                  |
| モデルの一時的rate limit | 分類可能な場合のみ上限付きbackoff。attemptを記録          |
| tool未dispatch           | 未開始として閉じ、必要なら新しい呼び出しを提案            |
| brokerに完了receiptあり  | 既存結果を取り込み、再実行しない                          |
| tool開始後に結果不明     | `outcome-unknown`として閉じ、外部状態確認か人間判断を要求 |
| 圧縮生成失敗             | 元contextを保持。無変更のoverflow要求を無限再試行しない   |
| DB commit / flush失敗    | runを正常終了と表示せず、外部副作用を伴う新規実行を停止   |

外部コマンド実行とローカルDB commitは単一transactionにはできない。call IDだけでexactly-onceを保証しない。idempotency keyを持たない外部操作は結果不明が残り得る。

### 4.5 コンテキスト管理

順序は「不要な大出力をartifact参照へ変換 → 古いtool出力の可逆な短縮 → 確定済み範囲の要約」とする。直近の入力、実行中のtool、call / result対応を保持する。

予算はモデルごとに`contextWindow - outputReserve - safetyMargin`で計算し、tool schemaや画像も含める。固定の16k reserveを全モデルへ機械的に当てはめない。

圧縮は元イベントを書き換えず、対象seqとsummaryを新イベントとしてcommitする。生成中に対象contextが変わったら再検証し、不整合な置換を採用しない。summaryは生成結果であり、元の指示と同じ権威を与えない。

初期の長期記憶は、出典・更新日・適用projectを持つ明示的なtask noteだけとする。前回の推測や古い決定を、検索で見つかったという理由だけで現在の要件に昇格させない。

## 5. ツール実行と安全境界

### 5.1 固定pipeline

```text
schema validation
  → immutable action normalization
  → hard deny guards
  → policy: allow / deny / ask
  → one-shot approval when required
  → revalidate authorization, policy and target state
  → sandboxed dispatch
  → immutable execution outcome
  → model-visible result projection
  → durable commit
```

pluginの結果整形は許可しても、監査上の実行成否・拒否理由・receiptは変更させない。`deny`を後段hookで`allow`へ変えることは禁止する。

承認は`sessionId / callId / actionHash / project / policyVersion / expiry`へ結び付ける。承認後に引数が変われば再承認する。clientの切断・失効・重複応答・期限切れ・承認者不在では許可しない。承認待ちはrun全体のホスト操作lockを保持せず、停止や他sessionの操作を可能にする。

書き込みはexpected-content hashを要求し、古いread結果による上書きを防ぐ。pathの文字列prefix検査だけでなく、実行環境側で解決したroot、symlink / Windows reparse point、TOCTOUを扱う。shellの副作用はファイル編集toolの検査だけでは防げないため、subprocess側にも隔離を適用する。

### 5.2 最初のツールと並列化

初期toolは`read`、`list`、`search`、`edit`、`write`、`run`に限定する。検索の実体にripgrep等を利用しても、モデル向け契約は独自に保つ。

- 読み取りtoolのみ、小さい上限を設定して並列化する。
- `edit` / `write` / `run`はsession内で直列化する。
- 同一projectの複数session間でも書き込み競合を検出する。必要な実行区間だけproject mutation lockを取る。
- `run`は原則として副作用不明と扱う。コマンド文字列の見た目だけでread-only判定しない。
- 出力byte数、実行時間、モデルstep数、費用の上限を持つ。価格不明でもstep / token / 時間上限は維持する。
- キャンセルは「要求済み」と「停止確認済み」を分ける。子process treeの終了を確認できなければ停止成功とは表示しない。

### 5.3 隔離の保証を表示する

Backendは一つの安全フラグではなく、`filesystem`、`network`、`process`、`credentials`ごとに`enforced / partial / unavailable`を報告する。

Windowsを含む各OSでbackendを実機検証するまでは、名前だけの「安全モード」を提供しない。要求した隔離が使えない場合はfail closedとし、通常実行へ黙ってfallbackしない。通常実行を選べる機能を設ける場合は、ホスト利用者による明示的な設定と監査を別途要求する。

API keyはホスト側の資格情報storeからモデル通信にだけ渡し、tool processの継承環境から除外する。session DBやartifactにもソースコード・機密情報が入るため、保存領域のアクセス制御、保存期間、明示的削除、export時の確認が必要である。診断telemetryへ会話本文を既定で送らない。

### 5.4 pluginは能力境界と同義ではない

初期版のin-process拡張は信頼した同梱コードだけとする。権限manifestは配布物の説明には使えても、それだけでNode.jsのfilesystemやnetworkアクセスを制限できない。

第三者pluginを追加する段階では、秘密情報を渡さない別process、OS隔離、broker経由の限定APIを組み合わせる。単なるworkerや別processを安全なsandboxと呼ばない。プロジェクト内のpluginや実行可能設定も、project trustの確認なしに自動ロードしない。

## 6. ReCodexへの段階導入

現在の`host.rs`はCodex固有の`thread/*` / `turn/*`、承認、project公開範囲、有界replayを扱う。履歴と認証の正本はCodexにある。UIもCodexイベントを前提としているため、単に起動する実行ファイルを差し替えるだけでは移行できない。[R1][R2]

### 6.1 外側のbackend契約

UI向けに小さな`AgentBackend`契約を設け、`CodexBackend`と`NativeHarnessBackend`を並存させる。

共通化するのはsession作成・参照、run開始・停止、model一覧、承認応答、event購読・snapshotである。高度な分岐やprovider固有機能はcapabilityとして明示し、すべてのbackendが対応しているように見せない。Codexの内部tool実行はCodex側が所有し、新brokerで統制済みとは表示しない。

sessionには`backendKind`を固定する。既存Codex sessionをnative sessionへ切り替えて同じ履歴をそのまま再開することは保証しない。移行はユーザーが確認した要約・添付情報から新sessionを作る明示的操作とする。

モデルAPIの認証とCodex / ChatGPTのサインインは別の契約である。現在のCodex資格情報を独自APIクライアントへ流用できる前提にしない。

### 6.2 再接続とLAN

- Runtimeは一つのHostに所有させ、LAN clientから直接公開しない。
- Hostのepoch / sequenceと、RuntimeのsessionSeqを別フィールドにする。片方をもう片方の永続cursorとして扱わない。
- snapshotには、その状態が含む確定seqを付け、以降のevent購読との間で欠落しない手順をprotocol化する。
- event IDで重複を排除し、replay範囲外ならsnapshotへ戻る。
- Host再起動後の実行継続は初期版で約束しない。未完了runをinterruptedとして復旧し、結果不明のtoolを確認する。
- 共有取消後は要求だけでなくsnapshot、artifact、replayも認可し直す。別projectの本文や絶対pathを漏らさない。
- stdioのstdoutはprotocol専用、診断はstderrとする。未知のmajor version、過大frame、未知の必須機能は拒否する。

UIを閉じても実行させるdaemon化は別要件とする。まず現在と同様に、ホストアプリの寿命を実行環境の寿命とする。

## 7. 実装順序と合格条件

| 段階                | 実装範囲                                                          | 次へ進む条件                                       |
| ------------------- | ----------------------------------------------------------------- | -------------------------------------------------- |
| 0: 境界整理         | backend契約、既存Codex adapter、protocol schema                   | 現行のchat・承認・LANテストが退行しない            |
| 1: read-only MVP    | native loop、1つのAPI provider、read系tool、永続log、停止、budget | 再起動・重複送信・snapshot復元の契約テストが通る   |
| 2: 変更操作         | Rust broker、edit / write / run、承認、sandbox probe、receipt     | 対象OSで隔離試験が通る。隔離不可時に実行しない     |
| 3: 長時間運用       | artifact、圧縮、モデル切替、読み取り並列、再接続の負荷検証        | call/result不整合・二重実行・event欠落がない       |
| 4: 実需に応じた拡張 | MCP、子agent、remote execution、外部plugin                        | 既存境界では満たせない具体的要件と評価ケースがある |

MVPの目標は「単一タスクを、安全に止められ、確定した状態から説明可能に再開できること」であり、Pi / DeepSeekの全機能互換ではない。

### 必須の障害・安全性テスト

- assistant確定前、tool受付前、tool実行後、結果commit前、run終端前でprocessを強制終了する。
- 同一command ID、同一call IDの再送、異なる引数でのID再利用を検査する。
- 承認直後の取消、別clientの二重応答、失効deviceからの応答を拒否する。
- 部分JSON、出力上限終了、未知tool、tool結果schema違反を実行前または確定前に拒否する。
- symlink / reparse point経由のroot逸脱、競合編集、shell経由の書き込みを検査する。
- subprocessへの秘密環境変数の継承、許可外networkアクセスを対象OSで検査する。
- DB故障、満杯disk、遅いclient、replay欠落、圧縮中のcancelと入力追加を検査する。
- 拒否済み操作がpluginによって許可へ変わらないことを検査する。

### 実モデル評価

固定fixtureのバグ修正・小機能追加・リファクタリング等を、まず20件程度用意する。比較対象は既存Codex運用、必要ならPi、DeepSeek、独自MVPとするが、同じモデルを利用できないbackendはハーネス単独の比較から除外する。

モデル、reasoning、tool、sandbox、token上限をそろえた比較と、各製品の推奨設定での比較を別集計にする。複数回実行し、次を記録する。

- 隠した受入テストによる完了率、誤変更・意図しない副作用。
- 成功1件あたりの費用、総token、再試行、圧縮回数。
- 初回表示、総所要時間、tool待ち時間、停止確認までの時間。
- 使用した設定・adapter版と、replayで復元できた要求の割合。

最初のrelease gateでは、拒否・権限・二重実行に関する契約テストの失敗を許容しない。速度や費用の改善率は測定後に決め、設計段階で数値を捏造しない。

## 8. 採用しない案と代替

| 不採用案                            | 理由                                                | 代替                                              |
| ----------------------------------- | --------------------------------------------------- | ------------------------------------------------- |
| PiとDeepSeekを丸ごと合体            | 2つの状態機械、履歴形式、拡張lifecycleが重複する    | 独自kernel＋モデル通信libraryの限定再利用         |
| DeepSeekの全面fork                  | previewの変更追従と全plugin基盤の運用まで必要になる | 能力境界と不変条件を参考に独立実装                |
| 初期からPi SDKに独自正本logを重ねる | SessionManagerと独自journalが競合しやすい           | SDKを使うならPi所有sessionを包む試作に限定する    |
| 最初から純Rustで全providerを実装    | provider差分の保守が初期目標を圧迫する              | TypeScriptでadapterを再利用し、RustをOS境界に集中 |
| 任意コードによるpolicy hook         | 後段変更や直接OSアクセスで境界を迂回できる          | 固定guardと限定的な拡張API                        |
| LLMに承認や安全性を判断させる       | prompt injectionと誤判定を防御境界にできない        | ホストの決定的policy＋必要時の人間承認            |
| 自律multi-agentを初期標準にする     | 費用・競合編集・停止・復旧の問題が増える            | 単一agentを評価後、read-only委譲から追加          |

**最初に作るべきものはplugin marketplaceではなく、read-onlyの独自runをReCodexから開始し、止め、再接続して確定履歴を再取得できる最小の縦断実装である。**

## 参考資料

リンクは分析対象コミットに固定している。ローカルでは同じ相対pathを`refs/pi/`または`refs/deepseek-harness/`配下で参照できる。

- [P1] — Pi agent coreの構造、turn、queue、tool hooksと並列実行。
- [P2] — Piの実行ループとモデル向けcontext変換の実装。
- [P3] — Pi session形式、分岐、圧縮、context edit。
- [P4] — Pi compactionの境界規則と失敗時の処理。
- [P5] — Pi SDKとCLIへの統合境界。
- [P6] — Piの信頼境界とサンドボックス非搭載の方針。
- [D1] — DeepSeek全体構成とmodel-visible logの原則。
- [D2] — DeepSeek loopの永続化、復旧、既知の制約。
- [D3] — DeepSeek tool pipeline、monotonic guardの実装。
- [D4] — DeepSeekの一回限りの承認とfail-closedな結果。
- [D5] — DeepSeek sandboxの対象範囲とpartial enforcement。
- [D6] — DeepSeek公式の安全性に関する注意。
- [D7] — DeepSeekのpi-ai adapter、provider対応と依存版パッチの制約。
- [D8] — DeepSeek subagentの能力分離。
- [D9] — DeepSeekのprocess外SDK統合。
- [D10] — DeepSeek compactionのログと能力境界。
- [R1] — ReCodexのCodex固有request処理。
- [R2] — ReCodexの現行アーキテクチャと運用仕様。

[P1]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/agent/README.md
[P2]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/agent/src/agent-loop.ts
[P3]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/coding-agent/docs/session-format.md
[P4]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/coding-agent/docs/compaction.md
[P5]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/packages/coding-agent/docs/sdk.md
[P6]: https://github.com/earendil-works/pi/blob/9376c8d17cd37f05d4f439f892026b03769aa789/SECURITY.md
[D1]: https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/architecture.md
[D2]: https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/agent-loop/README.md
[D3]: https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/tools/src/pipeline.ts
[D4]: https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/subsystems/approval.md
[D5]: https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/subsystems/sandbox.md
[D6]: https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/SAFETY.md
[D7]: https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/llm/llm-pi-ai/README.md
[D8]: https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/subagent/README.md
[D9]: https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/sdk/README.md
[D10]: https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/subsystems/compaction.md
[R1]: ../src-tauri/src/host.rs
[R2]: ../README.md
