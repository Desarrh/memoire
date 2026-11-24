"""Logo de Mémoire : carré crème arrondi, presse-papiers bleu nuit, coin de capture bleu.
Usage : py -3 assets/make_icon.py"""
from pathlib import Path
from PIL import Image, ImageDraw
CREAM=(242,230,204,255); NAVY=(19,34,58,255); BLUE=(46,94,158,255); LIGHT=(214,200,170,255)
def draw(px):
    S=4*px; img=Image.new("RGBA",(S,S),(0,0,0,0)); d=ImageDraw.Draw(img); k=S/1024
    s=lambda v:int(v*k)
    d.rounded_rectangle([s(24),s(24),s(1000),s(1000)],radius=s(200),fill=CREAM)
    d.rounded_rectangle([s(230),s(190),s(720),s(840)],radius=s(60),outline=NAVY,width=max(1,s(60)))
    d.rounded_rectangle([s(360),s(130),s(590),s(250)],radius=s(40),fill=NAVY)
    for i,y in enumerate([380,500,620]):
        d.rounded_rectangle([s(330),s(y),s(620 if i<2 else 520),s(y+50)],radius=s(25),fill=LIGHT)
    # coin de capture
    w=max(1,s(70))
    d.line([(s(640),s(900)),(s(900),s(900)),(s(900),s(640))],fill=BLUE,width=w,joint="curve")
    return img.resize((px,px),Image.LANCZOS)
here=Path(__file__).parent
draw(256).save(here/"icon.png"); draw(64).save(here/"icon64.png")
draw(256).save(here/"icon.ico",sizes=[(16,16),(24,24),(32,32),(48,48),(64,64),(128,128),(256,256)])
