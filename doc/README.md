# ドキュメント

- [実装状況（M0〜M8）](MILESTONES.md)
- [Web アプリへの組み込み方](embedding.md)
- [Alto Engine 設計書（PDF）](<Alto Engine 設計書.pdf>)
- [版管理の拡張と最低限の画像編集](design/VERSIONING-AND-IMAGE-EDITING.md)
- [版管理・画像編集の実装検証（2026-11-03）](verification/VERSIONING-IMAGE-EDITING-2026-11-03.md)
- [未対応機能の段階実装の検証（2026-11-03）](verification/FOLLOWUP-STAGES-2026-11-03.md)
- [過去の版のタイルを必要時に読む](design/ON-DEMAND-HISTORY-OBJECTS.md)
- [未実装機能の設計と実装計画](design/backlog/00-ROADMAP.md)
- [ファジングと unwrap の監査（QA-1、2026-11-08）](verification/UNWRAP-AUDIT.md)
- [エンジンの修正・検証計画（2026-11-09）](design/2026-11-09-engine-hardening-plan.md)。結果は [ENGINE-HARDENING-RESULTS.md](verification/ENGINE-HARDENING-RESULTS.md)
- [`design/`](design/)：設計と実装計画
- [`verification/`](verification/)：実装結果・測定・調査の記録。総まとめは [RESULTS.md](verification/RESULTS.md)

記録が参照する測定データ（JSON・スクリーンショット・サンプル文書）は、スクリプトが読み書きするため、リポジトリ直下の [`verification/`](../verification/) に置いたままにしている。
