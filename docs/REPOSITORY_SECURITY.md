# Repository security setup

公開リポジトリの保護方針と設定手順です。CODEOWNERSやworkflowだけではGitHub側の保護は有効にならないため、サーバー側のrulesetと権限設定も維持してください。

- 所有者アカウントにパスキーまたは2要素認証を設定する。共同管理者・外部アプリには必要な権限だけを与える。
- mainのrulesetを有効化し、削除・force pushを禁止する。pull request、CODEOWNERS承認、古い承認の失効、会話の解決、`publication-check`成功を必須にする。
- 所有者本人は自分のPRを承認できないため、単独運用では所有者だけのPR経由bypassを設定する。他の利用者にはbypassを与えない。
- Actionsはread-only tokenを既定にし、ActionsによるPR作成・承認を無効化する。外部コントリビューターのworkflowは実行前承認を必須にする。
- 許可するActionsを制限し、コミットSHAで固定する。`pull_request_target`で外部PRのコードを実行しない。
- Secret scanning、push protection、Dependabot alerts、private vulnerability reportingを利用可能な範囲で有効化する。
- リリースタグの変更・削除をrulesetで制限する。npm/PyPI公開を追加する際は、所有者承認付きenvironmentと短命な認証を使う。
- 不要なWiki、Discussionsを無効にし、コラボレーターとインストール済みGitHub Appsの権限を確認する。

公開リポジトリは第三者が閲覧・forkでき、設定に応じてissueやPRを送れます。これらは書き込み権限ではありません。閲覧・複製も制限したい場合はprivate運用が必要です。

公開前のローカル検査:

社名など追加の禁止語は`PUBLICATION_DENY_TERMS`環境変数へカンマ区切りで指定できます。CIでは同名のrepository variableを設定します。未設定時は秘密情報・パス・生成物の標準検査のみを実施します。

```bash
python3 scripts/check-publication.py
cargo package -p document-svg --list --allow-dirty
```

検査は候補ファイルの名前とテキストを対象にし、秘密情報の検出を保証するものではありません。生成物、キャッシュ、検証用文書はGitと配布物に入れない運用とします。

公開サンプルは例外として、`samples/source/`の指定4ファイルのみ、`samples/provenance.json`のSHA-256と一致するときに許可します。テストでは生成スクリプトからの再現も確認します。ライセンス監査は`docs/LICENSE_AUDIT.md`を参照してください。
