# 独自エージェントハーネス設計案

## 0. この文書の位置付け

本書は、Pi AgentとDeepSeek Harnessの分析を基に、独自ハーネスの構造・責務・データ・動作契約を提案する設計書である。実装計画、リリース範囲、作業順序を定めるものではない。

前稿で記載したMVP、段階導入、機能の先送り、機能除外、および言語・保存方式・ReCodexへの適用を確定事項とした記述は撤回する。これらは利用者から指定された条件ではなかった。

記述を次の4種類に分ける。

| 区分     | 意味                                                                     |
| -------- | ------------------------------------------------------------------------ |
| 分析事実 | 参照したソースで確認した既存ハーネスの性質                               |
| 設計提案 | 独自ハーネスを構成するために提案する構造と契約                           |
| 技術選択 | 同じ論理設計を実現する実装方式の候補と、その成立条件                     |
| 未決事項 | 要件または利用者の判断が必要で、採否・設定・実装予定を確定していない事項 |

**以下の機能に設計を与えることと、それを実装対象として採用することは別である。** 本書は、工数を理由に機能を省略したり、機能を過剰・不要と判定したりしない。設計上の不変条件は、ある構成を選んだ際に整合性を保つための条件であり、製品の対象範囲を勝手に決めるための境界ではない。

## 1. 分析対象と確認できたこと

| 対象             | 固定したソース                                                      |
| ---------------- | ------------------------------------------------------------------- |
| Pi Agent         | `refs/pi`、`9376c8d17cd37f05d4f439f892026b03769aa789`               |
| DeepSeek Harness | `refs/deepseek-harness`、`ce7073d51254c26cd63b22a60835b8f948deb447` |
| ReCodex          | 現在の`README.md`と`desktop/src/host.rs`。統合先候補の分析に使用  |

Piの`runLoop`、モデル向けcontext変換、並列tool実行と、DeepSeekの`ReactLoopAgent.step`、`buildRequest`、`ToolExecutionPipeline.prepare`を確認した。実モデル比較、負荷試験、OS隔離の侵入試験は実施していない。速度・費用・解決率の優劣は未検証である。[P1][P2][D1][D2][D3]

### 1.1 比較

| 観点     | Pi Agentの分析事実                                      | DeepSeek Harnessの分析事実                                | 設計上の論点                                     |
| -------- | ------------------------------------------------------- | --------------------------------------------------------- | ------------------------------------------------ |
| 構成     | `pi-ai`、agent core、coding-agent層を分離               | Cordis上でモデル、tool、session、loopをpluginとして構成   | 実行状態の所有者と、交換可能な実装の契約を分ける |
| 実行単位 | coreのturnはモデル要求とtool処理に対応                  | stepがモデル要求とtool処理、turnが複数stepを含む          | 製品間で名称を同一視せず、独自用語を定義する     |
| 入力     | steeringとfollow-upに別の受付タイミング                 | inboxを永続イベントから投影                               | 受付、claim、割込み、取り消しを区別する          |
| 履歴     | JSONLの`id` / `parentId`による分岐木                    | append-only logとmodel-facing surface                     | 履歴、分岐、表示、モデル入力を別のviewとして扱う |
| context  | active branch、compaction、branch summary、context edit | 要求header・contextとsurface変更を記録                    | 実際のモデル入力を再構成できる記録を持つ         |
| tool     | schema検証、before/after hook、直列・並列実行           | executor、承認、monotonic guard、結果確定を分離           | 拡張処理と実行権限の決定を混同しない             |
| 並列性   | core既定は並列。直列指定toolがあるbatchは直列           | 並列上限とexclusive barrier                               | 競合する資源・副作用を表現してscheduleする       |
| 拡張     | tool、command、provider、event、resource等の拡張        | service依存、scope、破棄可能なeffect、profile、patch、HMR | 登録・依存・設定・交換・破棄を一体で設計する     |
| 外部統合 | in-process SDK、JSON、長寿命RPC                         | profileを起動するprocess外JSON-RPC SDK等                  | transportと実行意味論を分離する                  |
| 委譲     | 製品方針として標準機能を絞り、拡張で追加                | in-process、Codex、Claude Code、ACP等のprovider           | agent間連携を特定の実行製品から分離する          |
| 信頼境界 | ユーザー権限で動作し、標準sandboxなし                   | sandbox / approval seamあり。未監査のpreview              | policy、承認、OS隔離の保証範囲を個別に表現する   |

根拠: [P1][P2][P3][P4][P5][P6][D1][D2][D3][D4][D5][D6][D8][D9][D10]。

### 1.2 両者を単純な二者択一にできない理由

DeepSeek Harnessは複数provider対応に`pi-ai`を使うadapterを持つ。独自のloop・session・plugin基盤と、再利用したモデル通信は両立している。ただし依存版へのpatchやprovider固有制約もあるため、libraryを共有しているだけで同一の挙動になるわけではない。[D7]

Piの強みとして確認できるのは層の分離であり、「それ以外の機能は不要」という結論ではない。DeepSeekの特徴は構成可能性であり、「すべてをplugin化すれば安全性や永続性も保証される」という結論ではない。

DeepSeekにはroot shutdown時の終端イベント永続化や組み込みturn budgetに関する既知の制約が記されている。独自設計では、その制約の存在と、求める動作契約を分けて扱う。[D2]

## 2. 設計の中心概念

**提案する構造は「実行の意味論を所有する制御系」と「能力を提供する構成系」を分離するハーネスである。**

制御系は、何を受付済みとみなすか、どの履歴からモデル入力を作るか、誰が操作を許可できるか、いつ結果を確定するかを定義する。構成系は、モデル、tool、保存、記憶、agent、UI、実行環境の実装を提供する。

制御系も実装の交換対象にできる。ただしloop実装の交換は、既存runの状態契約やevent schemaとの互換性を検証する操作であり、toolを追加する操作とは区別する。

```text
Human / UI / CLI / SDK / Automation
                  |
          Application Gateway
       identity / commands / subscriptions
                  |
          Harness Coordinator
       session / run / task / agent lifecycle
                  |
  +---------------+----------------+----------------+
  |               |                |                |
Run Driver    Context Engine   Agent Coordinator  Command Service
  |               |                |                |
Model Gateway  Memory Service   Task Graph       Human Interaction
  |
Tool Scheduler ---- Authorization ---- Execution Environment
  |                       |              fs / process / network
Tool Registry         Approval Authority
  |
MCP / Code Execution / LSP / Terminal / Browser / Other Capabilities

Session Ledger / Artifact Store / Projection Engine / Telemetry
             ↑ 各サービスの確定した事実と派生view

Composition Runtime
  plugin / dependency / scope / config / HMR / disposal
             ↑ 各サービスの実装・登録・構成を管理
```

これは論理構造であり、箱ごとの別process化やmicroservice化を意味しない。process配置と言語は第13節の技術選択として分離する。

## 3. ドメインと識別子

| 概念         | 定義                                           | 関係                                       |
| ------------ | ---------------------------------------------- | ------------------------------------------ |
| Workspace    | ファイル、実行環境、設定、アクセス制御の対象   | 複数session・task・agentから参照される     |
| Session      | 対話と実行事実の履歴を持つ単位                 | branch、run、message、artifactを関連付ける |
| Branch       | ある履歴位置から継続する履歴の系統             | context選択と比較の対象になる              |
| Goal         | 達成したい結果と判定条件                       | task graphとplanの根拠になる               |
| Task         | 成果物・制約・依存関係を持つ作業単位           | agentへの割当とrunを関連付ける             |
| Plan         | task構成と依存関係に対する版付き提案           | 承認・修正・実行状態とは別に記録する       |
| Agent        | persona、能力、権限、context方針を持つ実行主体 | 一つ以上のtaskを担当する                   |
| Run          | 入力・task・自動triggerによって起動する実行    | 複数stepとagentへの委譲を含む              |
| Step         | 一つのモデル要求と、その応答に伴うtool処理     | 複数attemptを持ち得る                      |
| Attempt      | モデル要求の個々の試行                         | 確定した要求snapshotと対応する             |
| ToolCall     | 正規化された一つの能力呼び出し                 | 承認、実行receipt、終端結果と対応する      |
| Artifact     | 出力、画像、差分、ファイルsnapshot等の内容     | 所有者・出典・アクセス権を持つ             |
| MemoryRecord | 再利用する知識・判断・好み等の記録             | 出典・適用範囲・版・失効状態を持つ         |
| Composition  | plugin、設定、provider等の組合せ               | 実行が参照した版を特定できる               |

識別子は`workspaceId`、`sessionId`、`branchId`、`taskId`、`agentId`、`runId`、`stepId`、`attemptId`、`callId`、`artifactId`等を区別する。transportの`requestId`と、再送を識別する`commandId`はこれらとは別に持つ。

## 4. サービスの責務と契約

| サービス              | 所有する状態・判断                    | 入出力と連携                               |
| --------------------- | ------------------------------------- | ------------------------------------------ |
| Application Gateway   | 接続主体、protocol交渉、command受付   | 認証済みcommand、応答、購読event           |
| Harness Coordinator   | sessionとrunのlifecycle               | driver、ledger、agent、compositionを束ねる |
| Run Driver            | 入力queue、step遷移、継続・停止       | context要求、model要求、tool batch         |
| Model Gateway         | route、能力検証、provider変換         | 正規化request、stream、usage、replay情報   |
| Context Engine        | モデル向けviewとその構築規則          | 履歴、記憶、tool schema、artifact、圧縮    |
| Session Ledger        | 確定イベントの順序と競合検出          | append、read、checkpoint、branch、export   |
| Projection Engine     | ログからの派生状態                    | transcript、task状態、usage、検索・UI view |
| Tool Registry         | schema、effect宣言、実装参照          | stepで可視なtool setを解決する             |
| Tool Scheduler        | 依存、競合資源、実行順、cancel        | batchを実行し、結果を呼び出しへ対応付ける  |
| Authorization Service | 主体・対象・操作に対するpolicy        | allow / deny / askと、その根拠・版         |
| Approval Authority    | 人間または外部権限主体の承認          | 一回の操作または明示されたgrantへの決定    |
| Execution Environment | 同じ実行環境のfs・process等           | 実行receipt、保証された隔離能力、出力      |
| Agent Coordinator     | 親子関係、割当、連絡、join、cancel    | task graphとagent providerを連携させる     |
| Memory Service        | 記憶の登録、検索、更新、失効          | 出典付き候補をcontextへ提供する            |
| Artifact Store        | 内容、参照、アクセス権、保存lifecycle | 大出力、添付、snapshot、成果物             |
| Composition Runtime   | plugin依存、scope、設定版、交換       | service登録とeffectの取得・破棄            |
| Human Interaction     | 質問、plan確認、承認の表示            | UIと制御処理を相関IDで接続する             |
| Telemetry             | trace、計測、診断用view               | 本文・秘密情報の取扱規則と独立した計測     |

状態の変更には所有サービスのcommandを使い、他サービスから内部stateを直接書き換えない。pluginで実装されたサービスにもこの契約を適用する。

## 5. 実行ループと入力制御

### 5.1 実行状態

runの状態を`queued / preparing / generating / executing / waiting / cancelling / completed / cancelled / failed / interrupted`として区別する。`waiting`には承認、人間の回答、子agent、外部job等の待機理由を持たせる。

stepは`prepared / requesting / streaming / dispatching / settling / closed`、attemptは`started / succeeded / failed / cancelled`等の個別状態を持つ。外部副作用の結果不明は`outcome-unknown`として記録し、runの成功と同一視しない。

これらの状態は単一の文字列へすべてを詰め込まず、run、attempt、toolそれぞれの状態を投影して表現する。

### 5.2 入力の意味

- **Prompt**: runを開始する入力。
- **Steer**: 現在のstepが確定した後の継続判断に入れる指示。
- **Follow-up**: 現在のrunの自動処理が完了した後に処理する入力。
- **Inject**: 次のモデル入力で参照するcontext。runを起動するかどうかは別の属性にする。
- **Cancel**: 対象活動の停止要求。停止確認とは別の事実として扱う。
- **Revise / Withdraw**: 未claim入力の変更・取り消し。既に処理へ取り込んだ入力の履歴は消さない。

入力は受付・claim・取り消しを記録し、再接続や再送によって二重処理しない。どの入力がどのstepに入ったかを追跡できるようにする。Piのqueue分離とDeepSeekのinbox投影を、この動作契約の参考にする。[P1][D2]

### 5.3 stepのシーケンス

1. 入力をclaimし、そのstepのcomposition・model route・policyを解決する。
2. Context Engineが履歴・記憶・能力宣言からrequest候補を構築する。
3. model能力、protocol制約、context容量を検証する。
4. 確定した意味的requestと変換版を記録し、attemptを開始する。
5. streamを受け、完了messageまたは失敗attemptとして確定する。
6. 完成したtool要求をschema検証し、schedule・認可・実行へ渡す。
7. callごとの終端結果を確定し、次step・待機・run終端を決定する。
8. 終端条件が成立し、必要な確定処理が完了した時点でrun完了を通知する。

継続条件にはtool結果、未処理入力、goal / taskの状態、planの判断、停止policyを含める。step数・時間・token・費用・権限の上限は設定可能な実行policyとして表現し、具体的な値や運用モードは未決とする。

## 6. 履歴、分岐、永続化

### 6.1 eventとview

履歴はappend-onlyの論理event列として扱い、表示transcript、モデルcontext、task graph、usage、検索indexをそこから投影する。追記のみという性質は、保存backendをJSONLに固定することを意味しない。

event envelopeはschema版、event ID、stream ID、stream内seq、因果関係、主体、対象、composition版、payloadを持つ。stream内の順序と、複数stream間の因果関係を分け、分散環境の時刻だけで全順序を推定しない。

同一streamへのappendは`expectedSeq`等の競合検出とwriter fencingで直列化する。物理的に単一processが書く方式も、複数processがstoreのtransactionで直列化する方式も、この論理契約を実現する候補となる。

### 6.2 記録する事実

- 入力受付、claim、取り消し、run / step / attemptのlifecycle。
- model選択、request snapshot、context構築に使ったsource、tool schema。
- assistant message、失敗・中断attempt、provider replay情報。
- tool要求、dispatch receipt、結果、承認とpolicy版。
- branch作成、context置換、compaction、元の内容への参照。
- plan版、task遷移、agent間message、委譲とjoin。
- memoryの採用・更新・失効、artifactの作成と参照。
- 使用量、診断上の失敗、composition切替。

モデルへ見せた内容と、表示・監査だけに使う内容を区分する。model-visibleな内容は、元event、artifact、変換版から再構成できるようにする。認証情報はrequest snapshotへ保存しない。[D1][D2]

### 6.3 分岐と比較

branchは基点となる確定履歴と、そこからのevent列を持つ。contextの巻き戻し、別案への継続、branch間diff、離れたbranchのsummary参照を扱う。[P3][P4]

履歴の分岐はファイル状態の巻き戻しとは異なる。ファイル環境も分岐させる場合は、worktree、filesystem snapshot、remote environment等の環境版をbranchへ関連付ける。成果物の統合はmerge proposalと検証結果として記録し、会話履歴を結合しただけでファイルも統合済みとは扱わない。

### 6.4 streamの永続性

streamの保存方式は、提供する保証に応じて選択する。

| 方式                   | 通知と永続化の関係                       | 障害後の契約                                      |
| ---------------------- | ---------------------------------------- | ------------------------------------------------- |
| 一時delta＋message確定 | deltaは未確定、完成messageのcommitで確定 | 未commit prefixは失われ得る。UIが未確定と表示する |
| chunk永続化            | chunkをcommitした範囲を購読者へ通知      | 確定済みprefixからreplayできる                    |
| checkpoint付きstream   | checkpoint間のdeltaは未確定              | 最後の確定checkpointまでを復元する                |

どの方式を採るかは未決である。いずれもUIの到達済みseqと永続化済みseqを混同せず、確定messageで途中表示を照合する。低速clientにはbackpressure、再取得要求、購読queueの制御を適用する。

### 6.5 副作用と復旧

実行環境は`callId`と正規化要求hashに対する受付・開始・完了receiptを保持する。会話ledgerはそのreceiptを参照し、復旧時に照合する。

| 状態                           | 復旧時の解釈                                       |
| ------------------------------ | -------------------------------------------------- |
| tool要求のみでdispatch受付なし | 実行開始を確認していない                           |
| 実行環境に完了receiptあり      | 結果を取り込み、同じ操作を重ねない                 |
| 開始receiptはあるが終端なし    | 結果不明。外部状態の照合、補償、再承認等の対象     |
| 外部側のidempotency keyあり    | 外部契約に従って同じ操作の結果を照会・再取得できる |
| storeのcommit失敗              | 正常完了の通知と区別し、未確定範囲を明示する       |

外部副作用とledger更新を同じtransactionへ入れられない場合、call IDだけではexactly-onceを保証できない。通信再試行と副作用再実行を別のpolicyにする。補償操作も新しい承認対象・監査対象のcallとして扱う。[D2]

## 7. コンテキストと長期記憶

### 7.1 Context Engine

contextの入力は、選択branch、実行中task、system instruction、persona、workspace instruction、memory候補、retrieval結果、tool宣言、添付artifactである。各要素に出典、適用範囲、権威、鮮度、変換履歴を持たせる。

構築結果には、モデルへ渡すmessage / tool schemaと、そのsource mapを含める。providerごとに変換する場合も、変換前の意味的requestと変換版を追跡する。互換APIという名称だけでsystem更新、画像、reasoning、tool callの挙動が同じと仮定しない。[P2][D7]

context容量はmodel能力と出力reserveから評価する。短縮、artifact参照、検索による再取得、summary、画像offloadは別々の変換として表現し、policyが組み合わせる。変換順序や保持配分は設定・評価対象であり、唯一の固定順序とはしない。

compactionは元履歴を消さず、対象範囲と生成summaryを関連付ける。tool call / resultの対応を保ち、生成中に対象が変わった場合は採用前に整合性を検証する。[P3][P4][D10]

### 7.2 Memory Service

| 記憶の種類            | 内容                               | 主な出典                             |
| --------------------- | ---------------------------------- | ------------------------------------ |
| Episodic              | 何を実行し、何が起きたか           | session event、task結果、実行receipt |
| Semantic              | projectの構造、仕様、検証済み知識  | 文書、コード、検証結果、明示的登録   |
| Procedural            | 手順、skill、反復作業の規則        | skill定義、承認された手順、実行記録  |
| Preference / Decision | 利用者の好み、決定、禁止事項       | 利用者の明示入力、承認された決定     |
| Working               | 現在のgoal、未解決点、作業上の仮説 | taskとrunの状態                      |

MemoryRecordには出典、作成主体、適用する主体・workspace・project、版、有効期間、機密区分、確認状態、supersede関係を持たせる。仮説と確定した指示を区別し、推測から抽出した内容を利用者の決定へ自動昇格させない。

登録・抽出・統合・訂正・失効・削除をそれぞれ操作として扱う。訂正は出典との関係を残し、削除要件がある場合は元artifactや検索indexにも伝播する。

全文検索、symbol / graph検索、embedding / vector検索はMemoryRecordを探すindexであり、記憶の正本や権威ではない。検索候補はアクセス権、適用範囲、鮮度、矛盾を検証してからcontextに入れる。検索方式の採否は未決とし、いずれも同じ出典付きrecordを返す契約にする。

## 8. ツール、コード実行、実行環境

### 8.1 能力の表現

ToolDefinitionはschema、説明、実装版、権限要求、資源の読み書き宣言、実行環境、cancel能力、出力契約を持つ。動的に可視性を変える場合は、stepが参照したtool setをsnapshotとして保持する。

能力の例はfilesystem、search、shell、継続terminal、LSP、browser、network、MCP、コードによるtool合成、artifact操作、agent委譲、memory操作である。この列挙は実装対象を限定するリストではない。

MCP adapterは外部toolのschema・名前・権限・接続lifecycleをRegistryへ接続する。コード実行は、sandbox内のprogramからtoolを呼ぶためのbindingを提供する。入れ子のtool callにも個別のcall IDと認可を適用し、親callとの因果関係を記録する。MCP接続先やコード実行機構を認可の迂回路にしない。

### 8.2 scheduleと並列性

schedulerは単にread / writeの2分類でなく、資源key、read / write / exclusiveのaccess、依存call、環境ID、同時実行可能性を基にscheduleする。

- 同一資源の競合書き込みは直列化またはtransactionで調停する。
- 異なる資源への操作は、backendの保証に応じて並列化できる。
- shell等で効果を特定できない操作はunknownとして扱い、排他実行、隔離された環境、明示的な並列許可等のpolicyで解決する。
- 外部状態が変わるため、read-onlyという宣言だけで再現性や無害性は保証しない。
- UIへの完了通知順と、モデル向け結果の並びを分け、call IDにより対応付ける。
- 複数agent間も同じ資源調停に参加させる。call待機中に人間との対話や停止commandまで塞ぐlockは保持しない。

### 8.3 実行pipeline

```text
schema validation → immutable action normalization
  → resource planning → authorization
  → approval when requested → revalidation
  → environment dispatch → execution receipt
  → authoritative outcome → model-visible projection
  → ledger commit
```

承認後の引数・主体・policy・対象資源の変更は、承認された操作と別の操作として評価する。認可上の拒否を後段の表示hookで許可に変更しない。監査上の結果とモデル向け表示の加工結果を別に保持する。[D3][D4]

### 8.4 Execution Environment

filesystem、subprocess、terminal、LSP等は同じ環境IDを参照する。local、SSH、container、VM、remote workerは、この契約を実現するproviderとして扱う。[D1]

環境はpath表記、workspace root、実行資格、network policy、processの寿命、artifact転送、隔離の実効能力を返す。remote pathをlocal pathとして解釈せず、許可されたrootの検証は実際に資源が存在する側で行う。

## 9. 権限、承認、信頼境界

### 9.1 権限モデル

主体には人間、接続device、service account、agent、plugin、remote workerを含む。権限は主体・操作・対象資源・環境・委譲経路・policy版に基づいて評価する。agentが別agentやtoolへ委譲しても、明示的な権限主体によるgrantなしに権限を拡張しない。

承認には対象call、action hash、scope、期限、決定主体を結び付ける。一回限りの承認と、範囲を持つ継続grantを区別する。継続grantには取消と再評価の契約を持たせる。承認channelの切断・回答競合・期限切れを、それぞれ明示的な結果にする。

どの操作に承認を求めるか、無人運用を許すか、どの資源を公開するかは運用policyの未決事項である。認可を必要とする構成で、回答がないことを許可と解釈する設計は採らない。

### 9.2 OS隔離とpluginの信頼

Piはユーザー権限を信頼境界とし、標準sandboxを持たない。DeepSeekのsandbox modeはfilesystem効果を対象とし、networkやprocess可視性を同じmodeで保証していない。Windows等ではpartial enforcementがある。これらは既存実装の事実であり、独自ハーネスの保証をそのまま導くものではない。[P6][D5][D6]

独自環境はfilesystem、network、process、credentialsごとに`enforced / partial / unavailable`と実際の制約を報告する。要求された保証を満たせない場合は、拒否、別環境の選択、権限主体への明示的なpolicy変更要求を返す。黙って弱い保証へ置き換えない。

plugin manifestは要求権限を説明するものであり、直接OSへアクセスできるコードをそれだけで隔離できない。信頼コードの同一process実行、限定APIを持つ隔離process、remote実行等を別のtrust classとして表現する。process分離とOS隔離を同義にはしない。

### 9.3 情報の扱い

資格情報の正本はcredential serviceとし、認証referenceを能力呼び出しへ結び付ける。toolの継承環境、plugin、telemetry、exportに秘密情報を渡すかは個別に制御する。

session、memory、artifactにも機密内容が入る。保存時・転送時の保護、アクセス権、保持期間、削除、export、監査閲覧を横断的な契約にする。機密を含む本文の保存と、本文なしの計測を同じ設定で一括許可しない。

## 10. plugin、構成、HMR

### 10.1 pluginの契約

pluginはservice provider、tool、context source、memory index、agent provider、command、UI renderer、policy contributor、projection等を登録できる。依存service、対応schema / protocol版、要求権限、scope、設定schema、提供能力を宣言する。

登録は所有pluginとcomposition版に結び付くhandleを返す。解除時はそのpluginが所有する登録・listener・resourceのみを解放し、同名の新しい登録を古いdisposerが解除しないようにする。DeepSeekのscoped registrationと可逆effectが参考になる。[D1]

### 10.2 構成と設定

compositionはplugin graph、provider binding、設定、policy reference、artifact / package版を持つ。各runは参照したcompositionを特定できる。

設定layerを使う場合、layerの順序、merge / replace / removeの意味、同名定義の衝突、設定値の出典を規定する。実効設定の表示では、何がどこから設定されたかを説明できるようにする。動的expressionを許すかは、設定の実行権限とともに技術選択として扱う。

### 10.3 lifecycleとHMR

lifecycleは検出、検証、依存解決、準備、登録公開、稼働、drain、破棄として表す。準備失敗時は公開前の状態へ戻し、部分登録を残さない。

HMRでは新しいcomposition世代を準備・検証し、切替点で公開する。実行中stepは取得済みの旧世代handleを保持し、後続stepまたはrunのどの境界で新世代へ移るかを交換policyで指定する。

stateful pluginは状態移行、旧世代の継続、または再起動要求を宣言する。移行失敗時は旧世代を維持できるようにする。ただしpolicy取消やcredential失効は通常のコード世代固定とは別に扱い、実行直前にも有効性を確認する。

loop、ledger、認可authorityの交換には、それぞれ状態schema・writer所有権・権限連続性の検証が必要になる。UI rendererの交換と同じ保証で扱わない。HMRの提供形態や対象componentの採否は未決であり、設計契約として交換条件を定義する。

## 11. 複数agent、plan、task連携

### 11.1 agent provider

agentは同一runtime、隔離process、remote worker、外部ハーネスのproviderで実現できる。外部ハーネスadapterには、対応する停止、履歴、tool統制、権限委譲の能力を宣言させる。DeepSeekのsubagent familyは、この分離の実例となる。[D8]

親agentは委譲task、入力artifact、期待する成果、検証条件、権限scope、context継承方針を指定する。子agentは独自のrun / sessionを持ち、親への参照を保持する。親の履歴を全部複製する方式、特定snapshotを継承する方式、必要資料だけ渡す方式をcontext方針として扱う。

### 11.2 planとtask graph

planは版付きの提案として保存し、task依存、担当、成果物、受入条件を持つ。人間やagentによる修正は新しい版として扱い、実行中taskへどの変更が適用されたかを記録する。

task graphは実行依存を表し、agent間messageの通信graphとは分ける。待機依存の循環を検出し、join、timeout、cancel、失敗伝播、再割当の規則を持つ。role名やleaderの有無は構成で表現し、単一の組織形態に固定しない。

### 11.3 共同作業と成果統合

複数agentは共有資源への直接書き込み、隔離環境での作業、差分proposalの提出等の共同作業modeを選択できる。共有書き込みは第8節のschedulerで調停し、隔離成果の統合はmergeと検証を明示的なtaskとして扱う。

親runの停止時に子を停止するか、独立jobとして存続させるかはownership policyにする。権限grant、停止伝播、使用量の親子集計は同じ親子関係に基づいて追跡する。具体的なagent数・role・自律度・実装対象は未決である。

## 12. UI、protocol、daemon、分散配置

### 12.1 protocol

UI、CLI、SDK、自動化は共通command契約を使う。transportはstdio、local socket、WebSocket、HTTP等へ対応付ける。

| command群           | 操作の例                                           |
| ------------------- | -------------------------------------------------- |
| session / branch    | 作成、参照、分岐、比較、export                     |
| run / input         | 開始、steer、follow-up、入力編集、cancel、状態照会 |
| goal / task / agent | plan提案、割当、委譲、message、join、停止          |
| interaction         | 質問回答、承認、grant取消                          |
| composition         | 実効構成参照、検証、切替、plugin状態               |
| memory / artifact   | 登録、検索、参照、訂正、失効、削除                 |
| subscription        | snapshot、cursorからのreplay、live購読             |

受付成功と実行完了を別の応答にする。再送可能なcommandはcommand IDと内容hashを関連付け、同じIDで異なる操作を受け付けない。schema / protocol版と能力の交渉により、未対応操作を明示する。

snapshotには包含する確定seqを持たせ、以降の購読との間でeventが欠けない契約にする。event IDで重複を識別し、replay範囲外やprojection版の非互換時はsnapshotへ戻す。

### 12.2 UI

UIはtranscript、branch、task graph、agent状態、tool活動、plan、memory出典、artifact、権限と承認をそれぞれviewとして扱う。表示操作で正本eventを直接書き換えない。

renderer拡張は対応するevent / artifact型と版を宣言する。rendererが存在しない場合も、正本データと汎用表示・exportは失わない。TUI、desktop、Web間で表示能力が異なることをprotocol capabilityとして扱う。

### 12.3 daemonとremote運用

実行ownerの寿命をUI接続の寿命から独立した概念にする。desktop processがownerになる配置、常駐daemon、remote serviceはそれぞれdeployment profileとして表現する。どれを提供するかは未決である。

実行ownerの再起動では、writer lease / fencingを取得し、ledgerと実行receiptを照合してrun状態を回復する。実行中processが別workerで存続している場合は再attachできる契約を設ける。存続を確認できない活動を無条件で再起動しない。

分散配置ではidentity、tenant / workspace scope、credential境界、event stream、artifactアクセスを一貫して扱う。LANとクラウドで同じ認証強度や接続元を仮定しない。multi-tenant運用の採否は未決だが、主体と資源scopeをprotocolから欠落させない。

## 13. 技術選択とReCodexへの対応付け

### 13.1 言語・process配置

| 候補                                      | 論理設計との対応                                | 判断に必要な条件                                   |
| ----------------------------------------- | ----------------------------------------------- | -------------------------------------------------- |
| TypeScript runtime＋native execution host | model / plugin処理とOS実行境界を別processに配置 | JS pluginの契約、IPC、配布環境、隔離要件           |
| Rust中心のruntime                         | 制御系と実行環境をRustで表現し、adapterを接続   | provider / pluginとのABIやprotocol、非同期実行契約 |
| 単一runtimeへのSDK埋込み                  | applicationがruntimeの所有者になる              | application権限とplugin権限を同じ境界に置けるか    |
| daemon / remote service                   | applicationはclient、serviceがrun所有者         | 認証、tenant、接続断、復旧、worker所有権           |

言語・process配置は未決である。`pi-ai`再利用はModelAdapterの候補であり、独自型との変換、native replay、認証、依存版を検証する。外部libraryの利用は、sessionやloopもそのlibraryの所有にすることを意味しない。

### 13.2 保存方式

| 候補                     | 表現しやすい性質                      | 検証する契約                                      |
| ------------------------ | ------------------------------------- | ------------------------------------------------- |
| JSONL等のappend log      | event列の直接保存と移植               | writer競合、破損tail、index、checkpoint、機密削除 |
| SQLite等のembedded DB    | local transaction、eventとindexの整合 | process間writer所有権、flush、復旧、backup        |
| server DB                | 複数runtimeからのtransactionとquery   | tenant隔離、競合制御、availability、schema移行    |
| remote log＋object store | event streamと大きなartifactの分離    | 参照と内容の整合、順序、再取得、アクセス制御      |

いずれもSession LedgerとArtifact Storeの契約に接続する。正本eventと検索indexの役割を分ける。保存backend、暗号化方式、保持期間、削除要件、永続化modeは未決であり、SQLiteに固定しない。

### 13.3 ReCodexへの対応付け

現在のReCodexはRust hostがCodex固有の`thread/*` / `turn/*`、project認可、承認、有界replayを扱い、履歴・認証の正本はCodexにある。[R1][R2]

統合する場合、Application Gatewayを現在のhostへ対応付ける構成、独自daemonのclientへ変更する構成、既存Codexと独自runtimeをbackend adapterで接続する構成が考えられる。既存構造の維持・置換・並存のいずれも、この設計書で採用確定とはしない。

backendごとに、履歴読出し、branch、停止、model変更、tool統制、資格情報の扱いを能力として宣言する。外部Codexが実行するtoolを、独自Execution Environmentが統制しているとは表示しない。

session移行は、message・artifact・provider replay情報・実行receipt・認証の各互換性を分けて評価する。履歴import、新sessionへの要約移送、完全継続を別の操作として設計し、検証されていない完全継続を保証しない。Codex / ChatGPTのsign-inと、独自model APIの認証を同一視しない。

## 14. 設計を検証する観点

ここで示すのは設計契約を確認する観点であり、実装順序やrelease gateではない。

| 契約          | 検証する性質                                                |
| ------------- | ----------------------------------------------------------- |
| 入力と実行    | 再送で二重claimしない。cancel要求と停止確認を区別する       |
| request再構成 | model-visible内容がsource・artifact・変換版から追跡できる   |
| tool整合性    | callと結果の対応、未開始と結果不明の区別が保たれる          |
| 永続化        | commit前後の障害で確定範囲を誤表示しない                    |
| 分岐          | 履歴分岐と実環境の変更・巻戻しを混同しない                  |
| plugin / HMR  | 交換失敗時に部分登録を残さず、旧handleが新登録を破壊しない  |
| agent連携     | 委譲で権限が拡張されず、停止・join・成果統合が追跡できる    |
| memory        | 出典・訂正・失効・削除が検索とcontextへ反映される           |
| 認可          | 承認対象と実行対象が一致し、取消を実行直前に検証する        |
| 分散          | owner交代時の旧writerを排除し、再接続で欠落・二重処理しない |
| 情報境界      | 別主体のartifact・memory・replayや秘密情報を漏らさない      |

機能評価では、同じmodel、tool、実行環境、権限、context設定での比較と、各製品の構成全体としての比較を分ける。完了率、成果物の正しさ、副作用、再構成可能性、停止・復旧、資源使用量を計測する。評価件数・合格値・目標値は未決である。

## 15. 決定が必要な事項

以下は未設計機能や不採用機能の一覧ではなく、上記の構造を具体化する際の未決事項である。いずれも実装予定を意味しない。

| 項目            | 未決の内容                                               |
| --------------- | -------------------------------------------------------- |
| 利用形態        | desktop、CLI、SDK、daemon、remote serviceの提供形態      |
| 利用主体        | 単一利用者、共有workspace、multi-tenantとその権限要件    |
| 技術基盤        | 言語、process配置、provider library、plugin実行形式      |
| 保存            | backend、streamの確定単位、保持・暗号化・削除要件        |
| 拡張            | plugin配布・署名・trust class、HMRの対象と切替policy     |
| agent連携       | 自律度、role、task所有権、共同編集と成果統合mode         |
| 記憶            | 登録権限、確認規則、検索方式、scope、失効と訂正の運用    |
| 実行環境        | local / remote等のproviderと要求する隔離保証             |
| 人間との連携    | plan承認、質問、操作承認、継続grantのpolicy              |
| ReCodexとの関係 | 統合するか、どの対応付けを採るか、既存backendの扱い      |
| 実装対象と計画  | 各機能の採否、実装範囲、順序、日程。設計とは別に判断する |

**設計上の結論は、状態・履歴・権限の契約を明確にしたうえで、plugin、HMR、複数agent、記憶、保存、remote実行を同じ体系で接続する構造である。どの機能を実装するか、どの順番にするかは、この結論から自動的には決まらない。**

## 参考資料

リンクは分析対象コミットに固定している。ローカルでは同じ相対pathを`refs/pi/`または`refs/deepseek-harness/`配下で参照できる。Pi側の調査詳細は[Pi ハーネス基盤 調査報告](pi-harness-analysis.md)に分離している。

- [P1] — Pi agent core、turn、queue、tool hooks、並列実行。
- [P2] — Pi loopとモデル向けcontext変換。
- [P3] — Pi session、分岐、context edit。
- [P4] — Pi compactionとbranch summary。
- [P5] — Pi SDKと統合境界。
- [P6] — Piの信頼境界とsandbox方針。
- [D1] — DeepSeek全体構成、plugin、model-visible log。
- [D2] — DeepSeek loop、復旧、既知の制約。
- [D3] — DeepSeek tool pipelineとguard。
- [D4] — DeepSeek承認の契約。
- [D5] — DeepSeek sandboxの保証範囲。
- [D6] — DeepSeek公式の安全性に関する注意。
- [D7] — DeepSeekのpi-ai adapterと互換性の制約。
- [D8] — DeepSeek subagentの能力分離。
- [D9] — DeepSeekのprocess外SDK。
- [D10] — DeepSeek compaction。
- [R1] — ReCodexのCodex固有request処理。
- [R2] — ReCodexの現行構成。

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
[R1]: ../desktop/src/host.rs
[R2]: ../README.md
