// Mermaid init for nav docs
// Loaded by all pages that include diagrams.
// Pages with sequence diagrams should add class="mermaid-seq" to their container.
document.addEventListener('DOMContentLoaded', () => {
  if (typeof mermaid === 'undefined') {
    return;
  }

  const hasSequence = document.querySelector('.mermaid-seq') !== null;

  mermaid.initialize({
    theme: 'base',
    themeVariables: {
      darkMode: false,
      background: '#eef3ed',
      mainBkg: '#f5f8f4',
      primaryColor: '#f5f8f4',
      primaryTextColor: '#26332c',
      primaryBorderColor: '#bac9be',
      nodeBorder: '#bac9be',
      nodeTextColor: '#26332c',
      lineColor: '#1b7a4b',
      secondaryColor: '#e8f2ea',
      tertiaryColor: '#eef3ed',
      clusterBkg: '#eef3ed',
      clusterBorder: '#bac9be',
      fontFamily: 'ui-monospace,"SFMono-Regular",Consolas,monospace',
      fontSize: '13px',
      noteBkgColor: '#eef3ed',
      noteTextColor: '#26332c',
      noteBorderColor: '#bac9be',
      actorBkg: '#f5f8f4',
      actorTextColor: '#26332c',
      actorBorder: '#1b7a4b',
      labelTextColor: '#26332c',
      edgeLabelBackground: '#eef3ed',
    },
    flowchart: { htmlLabels: true, curve: 'linear', padding: 18 },
    ...(hasSequence ? { sequence: { actorMargin: 80, messageAlign: 'center' } } : {}),
  });
});
