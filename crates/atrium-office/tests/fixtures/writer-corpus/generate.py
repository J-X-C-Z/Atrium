from pathlib import Path
from datetime import date
from docx import Document
from docx.enum.style import WD_STYLE_TYPE
from docx.oxml import OxmlElement
from docx.oxml.ns import qn
from openpyxl import Workbook
from pptx import Presentation
from pptx.util import Inches
p = Path(__file__).parent / 'python'
p.mkdir(exist_ok=True)
word = Document()
s = word.styles.add_style('中文测试标题', WD_STYLE_TYPE.PARAGRAPH)
s.base_style = word.styles['Heading 2']
word.add_paragraph('预算测试', s)
a = word.add_paragraph('Original Word marker: ')
a.add_run('AtriumWordAnchor').bold = True
t = word.add_table(rows=2, cols=2)
for cell, txt in zip([c for row in t.rows for c in row.cells], ['项目', '金额', '研发', '150']):
    cell.text = txt
word.save(p / 'atrium-word.docx')
wb = Workbook()
ws = wb.active
ws.title = '预算表'
ws.append(['AtriumSheetAnchor', 120, '=B1*2'])
ws['B2'] = date(2026, 11, 21)
ws['B2'].number_format = 'yyyy"年"m"月"d"日"'
ws['B3'] = 0.25
ws['B3'].number_format = '0.00%'
ws.merge_cells('A4:B4')
ws['A4'] = '合并单元格'
wb.save(p / 'atrium-sheet.xlsx')
prs = Presentation()
slide = prs.slides.add_slide(prs.slide_layouts[5])
slide.shapes.title.text = '预算说明'
box = slide.shapes.add_textbox(Inches(1), Inches(2), Inches(5), Inches(1))
run = box.text_frame.paragraphs[0].add_run()
run.text = 'AtriumSlideAnchor'
run.font.bold = True
t = slide.shapes.add_table(2, 2, Inches(1), Inches(3), Inches(4), Inches(1)).table
for n, txt in enumerate(['项目','金额','研发','150']):
    t.cell(n//2, n%2).text = txt
prs.save(p / 'atrium-slides.pptx')
