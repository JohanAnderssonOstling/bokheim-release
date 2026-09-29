// A deterministic two-page PDF also usable by the full application test.
export function fixturePdf() {
    const stream = (text, color) => `q ${color} rg 72 500 468 130 re f Q\nBT /F1 24 Tf 72 700 Td (${text}) Tj ET`;
    const first = stream('First searchable page', '0.8 0.1 0.1');
    const second = stream('Second searchable page', '0.1 0.2 0.8');
    const objects = [
        '<< /Type /Catalog /Pages 2 0 R >>',
        '<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>',
        '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 6 0 R >>',
        '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 7 0 R >>',
        '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
        `<< /Length ${first.length} >>\nstream\n${first}\nendstream`,
        `<< /Length ${second.length} >>\nstream\n${second}\nendstream`,
        '<< /Title (PDF browser fixture) /Author (Bokheim test) >>',
    ];
    let pdf = '%PDF-1.7\n';
    const offsets = [0];
    objects.forEach((object, index) => { offsets.push(pdf.length); pdf += `${index + 1} 0 obj\n${object}\nendobj\n`; });
    const xref = pdf.length;
    pdf += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
    pdf += offsets.slice(1).map(offset => `${String(offset).padStart(10, '0')} 00000 n \n`).join('');
    pdf += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R /Info 8 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
    return Buffer.from(pdf);
}

