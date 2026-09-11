const fs = require('node:fs');
const path = require('node:path');
const YAML = require('yaml');

// The forms own the required sections; do not maintain a second schema here.
function readForms() {
  const directory = path.join(__dirname, '../ISSUE_TEMPLATE');
  const forms = fs.readdirSync(directory)
    .filter((file) => /\.ya?ml$/.test(file) && !/^config\.ya?ml$/.test(file))
    .sort()
    .map((file) => {
      const form = YAML.parse(fs.readFileSync(path.join(directory, file), 'utf8'));
      const sections = form.body
        .filter((field) => field.validations?.required)
        .map((field) => field.attributes?.label);
      if (!form.name || !sections.length || sections.some((label) => typeof label !== 'string' || !label.trim())
          || new Set(sections).size !== sections.length) {
        throw new Error(`Invalid required sections in ${file}`);
      }
      return { name: form.name, sections };
    });
  if (!forms.length) throw new Error('No issue forms found');
  return forms;
}

function readSections(body) {
  const sections = new Map();
  let lines;
  let fence;
  // Comments are template guidance, not answers or section boundaries.
  for (const line of body.replace(/<!--[\s\S]*?(?:-->|$)/g, '').split(/\r?\n/)) {
    if (fence) {
      const closing = /^ {0,3}(`{3,}|~{3,})[ \t]*$/.exec(line);
      if (closing && closing[1][0] === fence[0] && closing[1].length >= fence.length) {
        fence = undefined;
      } else {
        lines?.push(line);
      }
      continue;
    }
    const opening = /^ {0,3}(`{3,}|~{3,})/.exec(line);
    if (opening) {
      fence = opening[1];
      continue;
    }
    const heading = /^ {0,3}(#{1,3})[ \t]+(.+?)(?:[ \t]+#+)?[ \t]*$/.exec(line);
    if (!heading) {
      lines?.push(line);
      continue;
    }
    lines = undefined;
    if (heading[1].length === 1) continue;
    lines = [];
    const name = heading[2];
    if (!sections.has(name)) sections.set(name, []);
    sections.get(name).push(lines);
  }
  return sections;
}

function hasAnswer(lines) {
  return lines.some((line) => {
    const text = line.replace(/<br\s*\/?\s*>/gi, '').trim()
      .replace(/^(?:>[ \t]*)+/, '').trim();
    return text && !/^_No response_$/i.test(text)
      && !/^(?:[-*_][ \t]*){3,}$/.test(text)
      && !/^(?:[-*+]|\d+[.)])(?:\s+\[[ xX]\])?$/.test(text);
  });
}

function problems(issue, forms) {
  const errors = [];
  if (!/^[A-Z]/.test(issue.title) || /[\s.!?]$/.test(issue.title)
      || /^(?:\[[^\]]+\]|[a-z][a-z0-9_-]*(?:\([^)]*\))?!?:)/i.test(issue.title)) {
    errors.push('Title must start uppercase, have no [category] or type(scope): prefix, and end without whitespace or . ! ?');
  }

  const sections = readSections(issue.body || '');
  const matches = forms.map((form) => {
    const errors = [];
    let present = 0;
    for (const name of form.sections) {
      const entries = sections.get(name);
      if (!entries) {
        errors.push(`Missing section: ${name}`);
        continue;
      }
      present++;
      if (entries.length !== 1) errors.push(`Duplicate section: ${name}`);
      else if (!hasAnswer(entries[0])) errors.push(`Empty section: ${name}`);
    }
    return { ...form, errors, present };
  });
  const complete = matches.filter((match) => !match.errors.length);
  if (complete.length > 1) errors.push('Body must match one issue template, not multiple templates.');
  if (!complete.length) {
    const closest = matches.sort((a, b) => b.present - a.present)[0];
    if (closest.present) errors.push(...closest.errors.map((error) => `${closest.name}: ${error}`));
    else errors.push(`Use the required sections from one issue form: ${forms.map((form) => form.name).join(', ')}.`);
  }
  return errors;
}

module.exports = async ({ github, context, core }) => {
  // Read configuration and current content before performing any label mutations.
  const forms = readForms();
  const target = { ...context.repo, issue_number: context.payload.issue.number };
  const { data: issue } = await github.rest.issues.get(target);
  if (issue.pull_request || issue.state !== 'open') return;
  const errors = problems(issue, forms);
  const label = 'needs-template-fix';
  const labeled = issue.labels.some((item) => (typeof item === 'string' ? item : item.name) === label);

  if (errors.length && !labeled) {
    try {
      await github.rest.issues.getLabel({ ...context.repo, name: label });
    } catch (error) {
      if (error.status !== 404) throw error;
      try {
        await github.rest.issues.createLabel({
          ...context.repo, name: label, color: 'FBCA04',
          description: 'Issue title or required template sections need correction',
        });
      } catch (creationError) {
        // Different issues can race to create the repository-wide label.
        if (creationError.status !== 422) throw creationError;
        await github.rest.issues.getLabel({ ...context.repo, name: label });
      }
    }
    await github.rest.issues.addLabels({ ...target, labels: [label] });
  }
  if (!errors.length && labeled) {
    await github.rest.issues.removeLabel({ ...target, name: label });
  }

  core.summary.addHeading('Issue conventions');
  if (errors.length) {
    core.summary.addCodeBlock(errors.join('\n'), 'text');
    core.setFailed('Issue does not conform; see the workflow summary for required corrections.');
  } else {
    core.summary.addRaw('Issue title and required sections conform.\n');
  }
  await core.summary.write();
};
