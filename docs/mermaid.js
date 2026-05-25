// Mermaid init for nav docs
// Loaded by all pages that include diagrams.
// Pages with sequence diagrams should add class="mermaid-seq" to their container.
document.addEventListener('DOMContentLoaded', () => {
  const hasSequence = document.querySelector('.mermaid-seq') !== null;

  mermaid.initialize({
    theme: 'dark',
    themeVariables: {
      darkMode: true,
      background: '#0d1512',
      primaryColor: '#1a2a23',
      primaryTextColor: '#e0ece4',
      primaryBorderColor: '#2a4a3a',
      lineColor: '#34d399',
      secondaryColor: '#152220',
      tertiaryColor: '#0d1512',
      fontFamily: '"JetBrains Mono","Fira Code",monospace',
      fontSize: '13px',
      noteBkgColor: '#1a2a23',
      noteTextColor: '#e0ece4',
      noteBorderColor: '#2a4a3a',
      actorTextColor: '#e0ece4',
      actorBorder: '#34d399',
      labelTextColor: '#e0ece4',
      edgeLabelBackground: '#0d1512',
    },
    flowchart: { htmlLabels: true, curve: 'basis', padding: 20 },
    ...(hasSequence ? { sequence: { actorMargin: 80, messageAlign: 'center' } } : {}),
  });
});
