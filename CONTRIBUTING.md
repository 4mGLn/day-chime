# Contributing

이 프로젝트의 원본 코드와 에셋은 MIT License, Copyright (c) 2026 4mGLn으로 배포합니다.
의존성은 각 프로젝트의 라이선스를 따릅니다.

모든 게시 커밋은 암호학적으로 서명해야 합니다. 예:

```bash
git config commit.gpgsign true
git config user.signingkey YOUR_SIGNING_KEY
git commit -S -m "변경 내용"
git verify-commit HEAD
```

GitHub 계정에 검증된 서명키와 커밋 이메일을 등록해 GitHub에서도 Verified로 표시되게 하세요.
자동화는 릴리스 파일만 생성하며 저장소에 커밋을 자동 생성하지 않습니다.
main 브랜치에 게시된 전체 이력은 다음과 같이 확인할 수 있습니다.

```bash
for commit in $(git rev-list main); do
  git verify-commit "$commit" || exit 1
done
```

검증은 변경에 맞게 실행합니다.

```bash
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
node extension/tests/reader.test.cjs
node extension/tests/background.test.cjs
python3 -m unittest discover -s scripts -p 'test_*.py'
```

실제 근태 기록, 브라우저 쿠키, 인증키, 사용자 설정·상태 파일은 커밋하지 마세요.
브라우저 자동 수집 변경은 실제 로그인 환경과 모의 테스트의 검증 범위를 구분해 기록해 주세요.
