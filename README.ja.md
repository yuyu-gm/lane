# Lane

[English](README.md)

Lane は、並列作業するコーディングエージェントの Git worktree と統合を管理する CLI です。エージェントごとに branch と作業ディレクトリを作り、変更範囲と競合を確認してから親 branch に統合します。エージェントの起動は、利用している実行環境で行います。

## ビルド

Git 2.38 以上と Rust 1.90 以上が必要です。Windows では MSVC ツールチェーンと C++ ビルドツールを使います。

```powershell
cargo build --release --locked
./target/release/lane.exe schema
```

Windows x64 で検証しています。Linux/macOS は未検証です。実行時には Git が必要です。Python はテストやデモなどの開発用スクリプトに使います。

Python 3.11 以上があれば、検証を含む Windows 配布物を作成できます。

```powershell
powershell -NoProfile -File scripts/build.ps1
```

出力先は `dist/lane-<version>/` です。中の EXE を直接使うか、[インストール手順](docs/installation.md)に従って導入します。導入時には、エージェント設定の AGENTS.md と同じディレクトリに LANE.md を配置し、既存の指示を保持して参照を追加します。

## 使い方

作業対象の Git リポジトリで実行します。`lane` が PATH にない場合は、ビルドした EXE のパスで呼び出してください。

```text
lane init
lane spawn api --expected src/api --expected tests/api
```

返された `cwd`、`branch`、`base_commit` と制約を worker に渡します。作業を commit して書き込みを止めたら、親側で差分をレビューし、プロジェクトのテストを実行してから統合します。

```text
lane validate api
lane preflight api
lane integrate api
lane clean api --dry-run --delete-branch
lane clean api --delete-branch
```

各コマンドは JSON を出力します。終了コードと結果を確認してから次に進みます。コマンド・引数・終了コードの一覧は `lane schema` で取得できます。

複数の lane が完了している場合は、統合順序を指定して確認します。

```text
lane plan api ui
lane integrate-all api ui
```

`plan` は前の lane を統合した結果に次の lane を重ねます。個別の preflight が通っていても、lane 同士で競合する場合を検出できます。checkout、index、branch の参照は変更しません。

ignored なビルド成果物が残っている場合も、cleanup は保留します。`collect` で回収し、返された receipt を `clean --collection` に渡してください。成果物の回収と、2つの lane の競合を再現するデモがあります。

```powershell
python -X utf8 examples/demo.py --lane ./target/release/lane.exe
```

詳しい運用手順と JSON protocol は [docs/LANE.md](docs/LANE.md) を参照してください。

## 制約

統合・回収・cleanup 中は、worker と他の writer を停止してください。worktree は Git の object と参照を共有し、権限を隔離しません。Git の競合がなくても、コードレビューとテストは必要です。

一括統合は逐次処理です。後の merge が失敗しても、先に成功した統合は残る場合があります。再実行する前に receipt と現在の状態を確認してください。

## 開発

検証・配布手順とソースの配置は [CONTRIBUTING.md](CONTRIBUTING.md) にまとめています。

## ライセンス

[MIT](LICENSE)。
