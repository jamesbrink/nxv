# Documentation Writer Skill

Write clean, lint-passing Markdown documentation in the `./docs` directory.

## Guidelines

### Output Location

- ALL documentation files MUST be written to `./docs/`
- Use kebab-case for filenames: `indexer-guide.md`, `api-reference.md`
- Create subdirectories as needed: `./docs/guides/`, `./docs/reference/`

### Markdown Standards

Follow these rules for clean, lint-passing Markdown:

1. **Headings**
   - Use ATX-style headings (`#`, `##`, `###`)
   - Include blank line before and after headings
   - Don't skip heading levels (h1 -> h3)
   - Single H1 per document (the title)

2. **Lists**
   - Use `-` for unordered lists (consistent)
   - Use `1.` for ordered lists (let renderer number them)
   - Blank line before and after list blocks
   - Indent nested lists with 2 spaces

3. **Code Blocks**
   - Always specify language for fenced code blocks: ```bash, ```rust, ```nix
   - Use inline code for: commands, flags, filenames, values
   - Indent code blocks properly within lists (4 spaces)

4. **Links and References**
   - Use reference-style links for repeated URLs
   - Relative paths for internal docs: `[Guide](./guides/indexer.md)`
   - Check that referenced files exist

5. **Tables**
   - Align columns with pipes
   - Use `---` separator (at least 3 dashes)
   - Keep tables simple; use lists for complex data

6. **Line Length**
   - Wrap prose at ~100 characters for readability
   - Don't wrap code blocks, URLs, or tables

7. **Whitespace**
   - Single blank line between sections
   - No trailing whitespace
   - End file with single newline
   - No multiple consecutive blank lines

### Content Standards

1. **Structure**
   - Start with clear title and brief description
   - Include table of contents for long docs (>3 sections)
   - Use examples liberally
   - End with "See Also" or "Next Steps" where appropriate

2. **Tone**
   - Direct and concise
   - Active voice preferred
   - Technical but accessible
   - No fluff or filler

3. **Examples**
   - Show real commands that work
   - Include expected output where helpful
   - Explain non-obvious flags/options

### Validation

Before completing, mentally verify:

- [ ] File is in `./docs/` directory
- [ ] Single H1 title at top
- [ ] All code blocks have language specified
- [ ] No broken internal links
- [ ] Consistent list markers
- [ ] No trailing whitespace
- [ ] Ends with single newline
