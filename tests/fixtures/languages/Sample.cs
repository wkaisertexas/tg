namespace Acme.Tools;

public interface IReader
{
    string Name { get; }
    void Read();
    void Close() { }
}

public class Service
{
    private string name = "", alias = "";
    public string Title { get; set; }
    public Service() { }
    public void Render()
    {
        void Hidden() { }
        class Local { }
    }
    public abstract void Missing();
    public class Inner
    {
        public void Save() => Console.WriteLine("saved");
    }
}

public enum State
{
    Ready,
    Named = 2,
}

public delegate void Handler(string value);

public class Café
{
    public string Crème;
}
